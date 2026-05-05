use std::{env, sync::Mutex, thread};

use nucleus_protocol::{
    CpuCoreStat, CpuStats, DiskStat, HostStatus, MemoryStats, ProcessKillResponse,
    ProcessListResponse, ProcessListResponseMeta, ProcessSnapshot, SystemStats,
};
use sysinfo::{
    Disks, MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System,
    Users,
};

use crate::ApiError;

pub const DEFAULT_PROCESS_LIMIT: usize = 30;
pub const MAX_PROCESS_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryFrame {
    pub host_status: HostStatus,
    pub system_stats: SystemStats,
    pub processes_cpu: ProcessListResponse,
    pub processes_memory: ProcessListResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSort {
    Cpu,
    Memory,
}

impl ProcessSort {
    pub fn parse(value: Option<&str>) -> Result<Self, ApiError> {
        match value.unwrap_or("memory") {
            "cpu" => Ok(Self::Cpu),
            "memory" => Ok(Self::Memory),
            other => Err(ApiError::bad_request(format!(
                "unsupported process sort '{other}'; expected 'cpu' or 'memory'"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }
}

pub struct HostEngine {
    system: Mutex<System>,
}

impl HostEngine {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_cpu_all();

        Self {
            system: Mutex::new(system),
        }
    }

    pub fn host_status(&self) -> HostStatus {
        let mut system = self.system.lock().expect("host system mutex poisoned");
        refresh_system(&mut system);
        build_host_status(&system)
    }

    pub fn system_stats(&self) -> SystemStats {
        let current_user = current_user();
        let mut system = self.system.lock().expect("host system mutex poisoned");
        refresh_system(&mut system);
        build_system_stats(&system, &current_user, collect_disks())
    }

    pub fn processes(
        &self,
        sort: ProcessSort,
        limit: usize,
    ) -> Result<ProcessListResponse, ApiError> {
        let current_user = current_user();
        let users = Users::new_with_refreshed_list();
        let mut system = self.system.lock().expect("host system mutex poisoned");
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());
        system.refresh_memory();
        Ok(build_processes(&system, &users, &current_user, sort, limit))
    }

    pub fn telemetry_frame(&self, process_limit: usize) -> TelemetryFrame {
        let current_user = current_user();
        let users = Users::new_with_refreshed_list();
        let disks = collect_disks();
        let mut system = self.system.lock().expect("host system mutex poisoned");
        refresh_system(&mut system);

        TelemetryFrame {
            host_status: build_host_status(&system),
            system_stats: build_system_stats(&system, &current_user, disks),
            processes_cpu: build_processes(
                &system,
                &users,
                &current_user,
                ProcessSort::Cpu,
                process_limit,
            ),
            processes_memory: build_processes(
                &system,
                &users,
                &current_user,
                ProcessSort::Memory,
                process_limit,
            ),
        }
    }

    pub fn terminate_process(&self, pid: u32) -> Result<ProcessKillResponse, ApiError> {
        ensure_safe_kill_target(pid, std::process::id())?;

        let current_user = current_user();
        let users = Users::new_with_refreshed_list();
        let target = Pid::from(pid as usize);
        let mut system = self.system.lock().expect("host system mutex poisoned");
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[target]),
            true,
            process_refresh_kind(),
        );

        let process = system.process(target).ok_or_else(|| {
            ApiError::not_found(format!("process {pid} was not found or already exited"))
        })?;

        let owner = process_owner(process, &users).ok_or_else(|| {
            ApiError::forbidden(format!("unable to verify the owner for process {pid}"))
        })?;

        if owner != current_user {
            return Err(ApiError::forbidden(format!(
                "process {pid} belongs to '{owner}', not '{current_user}'"
            )));
        }

        match process.kill_with(Signal::Term) {
            Some(true) => Ok(ProcessKillResponse {
                killed_pid: pid,
                name: process.name().to_string_lossy().into_owned(),
                signal: "SIGTERM".to_string(),
            }),
            Some(false) => Err(ApiError::internal_message(format!(
                "failed to send SIGTERM to process {pid}"
            ))),
            None => Err(ApiError::internal_message(
                "SIGTERM is not supported on this platform".to_string(),
            )),
        }
    }
}

pub fn resolve_process_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_PROCESS_LIMIT);

    if limit == 0 {
        return Err(ApiError::bad_request(
            "process limit must be greater than zero".to_string(),
        ));
    }

    if limit > MAX_PROCESS_LIMIT {
        return Err(ApiError::bad_request(format!(
            "process limit must be {MAX_PROCESS_LIMIT} or lower"
        )));
    }

    Ok(limit)
}

fn refresh_system(system: &mut System) {
    system.refresh_memory();
    system.refresh_cpu_all();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());
}

fn build_host_status(system: &System) -> HostStatus {
    HostStatus {
        hostname: hostname(),
        cpu_usage_percent: round_tenths(system.global_cpu_usage()),
        memory_used_bytes: system.used_memory(),
        memory_total_bytes: system.total_memory(),
        process_count: system.processes().len(),
    }
}

fn build_system_stats(system: &System, current_user: &str, disks: Vec<DiskStat>) -> SystemStats {
    let cpu = CpuStats {
        load_percent: round_tenths(system.global_cpu_usage()),
        cores: system
            .cpus()
            .iter()
            .enumerate()
            .map(|(index, cpu)| CpuCoreStat {
                id: index,
                usage_percent: round_tenths(cpu.cpu_usage()),
                frequency_mhz: cpu.frequency(),
            })
            .collect(),
    };

    let memory = MemoryStats {
        total_bytes: system.total_memory(),
        used_bytes: system.used_memory(),
        free_bytes: system.free_memory(),
        available_bytes: system.available_memory(),
        used_percent: percent(system.used_memory(), system.total_memory()),
    };

    SystemStats {
        hostname: hostname(),
        current_user: current_user.to_string(),
        process_count: system.processes().len(),
        cpu,
        memory,
        disks,
    }
}

fn build_processes(
    system: &System,
    users: &Users,
    current_user: &str,
    sort: ProcessSort,
    limit: usize,
) -> ProcessListResponse {
    let total_processes = system.processes().len();
    let total_memory = system.total_memory();

    let mut processes = system
        .processes()
        .values()
        .filter(|process| process.pid().as_u32() > 1)
        .filter_map(|process| {
            let owner = process_owner(process, users)?;
            if owner != current_user {
                return None;
            }

            let (command, params) = split_command(process);

            Some(ProcessSnapshot {
                pid: process.pid().as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                command,
                params: truncate(params, 240),
                user: owner,
                cwd: process
                    .cwd()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                status: format!("{:?}", process.status()).to_lowercase(),
                cpu_percent: round_tenths(process.cpu_usage()),
                memory_bytes: process.memory(),
                memory_percent: percent(process.memory(), total_memory),
            })
        })
        .collect::<Vec<_>>();

    match sort {
        ProcessSort::Cpu => {
            processes.sort_by(|left, right| right.cpu_percent.total_cmp(&left.cpu_percent));
        }
        ProcessSort::Memory => {
            processes.sort_by(|left, right| right.memory_bytes.cmp(&left.memory_bytes));
        }
    }

    let matching_processes = processes.len();
    processes.truncate(limit);

    ProcessListResponse {
        processes,
        meta: ProcessListResponseMeta {
            total_processes,
            matching_processes,
            current_user: current_user.to_string(),
            sort: sort.as_str().to_string(),
        },
    }
}

fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::everything().without_tasks()
}

fn hostname() -> String {
    System::host_name().unwrap_or_else(|| "unknown".to_string())
}

fn current_user() -> String {
    env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn process_owner(process: &sysinfo::Process, users: &Users) -> Option<String> {
    process
        .user_id()
        .and_then(|user_id| users.get_user_by_id(user_id))
        .map(|user| user.name().to_string())
}

fn split_command(process: &sysinfo::Process) -> (String, String) {
    let parts = process
        .cmd()
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let command = parts
        .first()
        .cloned()
        .or_else(|| {
            process
                .exe()
                .map(|path| path.display().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| process.name().to_string_lossy().into_owned());

    let params = if parts.len() > 1 {
        parts[1..].join(" ")
    } else {
        String::new()
    };

    (command, params)
}

fn collect_disks() -> Vec<DiskStat> {
    let disks = Disks::new_with_refreshed_list();
    let mut items = disks
        .list()
        .iter()
        .filter(|disk| disk.total_space() > 0)
        .filter(|disk| is_relevant_mount(&disk.mount_point().display().to_string()))
        .map(|disk| {
            let total_bytes = disk.total_space();
            let available_bytes = disk.available_space();

            DiskStat {
                name: disk.name().to_string_lossy().into_owned(),
                mount_point: disk.mount_point().display().to_string(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                total_bytes,
                used_bytes: total_bytes.saturating_sub(available_bytes),
                available_bytes,
            }
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
    items.dedup_by(|left, right| left.mount_point == right.mount_point);
    items
}

fn is_relevant_mount(mount_point: &str) -> bool {
    matches!(mount_point, "/" | "/home" | "/System/Volumes/Data")
        || mount_point.starts_with("/home/")
        || mount_point.starts_with("/Volumes/")
        || mount_point.starts_with("/mnt/")
        || mount_point.starts_with("/media/")
}

fn percent(numerator: u64, denominator: u64) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        round_tenths((numerator as f64 / denominator as f64 * 100.0) as f32)
    }
}

fn round_tenths(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

fn truncate(value: String, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn ensure_safe_kill_target(pid: u32, current_pid: u32) -> Result<(), ApiError> {
    if pid <= 1 {
        return Err(ApiError::bad_request(
            "pid must be greater than 1".to_string(),
        ));
    }

    if pid == current_pid {
        return Err(ApiError::bad_request(
            "refusing to terminate the active Nucleus daemon process".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn rejects_zero_process_limit() {
        let error = resolve_process_limit(Some(0)).expect_err("limit 0 should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_excessive_process_limit() {
        let error = resolve_process_limit(Some(MAX_PROCESS_LIMIT + 1))
            .expect_err("too-large limit should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_reserved_pid_targets() {
        let error = ensure_safe_kill_target(1, 999).expect_err("pid 1 should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_self_termination() {
        let error = ensure_safe_kill_target(4242, 4242).expect_err("self kill should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn truncates_long_strings() {
        let value = truncate("abcdefghijklmnopqrstuvwxyz".to_string(), 5);
        assert_eq!(value, "abcde...");
    }

    #[test]
    fn handles_zero_denominator_percentages() {
        assert_eq!(percent(100, 0), 0.0);
    }
}
