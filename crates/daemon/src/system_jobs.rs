use std::{
    collections::{BTreeMap, BTreeSet},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use nucleus_protocol::{LocalJobDetail, LocalJobExit, LocalJobSchedule, LocalJobSummary};
use tokio::{process::Command, time::timeout};

const BACKEND_SYSTEMD_USER: &str = "systemd-user";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalJobControl {
    Enable,
    Disable,
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SystemScheduler {
    backend: SystemdUserScheduler,
}

impl SystemScheduler {
    pub fn systemd_user() -> Self {
        Self {
            backend: SystemdUserScheduler,
        }
    }

    pub async fn list_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        self.backend.list_jobs(allowlist_globs).await
    }

    pub async fn job_detail(
        &self,
        unit: &str,
        allowlist_globs: &[String],
    ) -> Result<LocalJobDetail> {
        self.backend.job_detail(unit, allowlist_globs).await
    }

    pub async fn control_job(
        &self,
        unit: &str,
        control: LocalJobControl,
        allowlist_globs: &[String],
    ) -> Result<LocalJobSummary> {
        self.backend
            .control_job(unit, control, allowlist_globs)
            .await
    }
}

#[derive(Debug, Clone, Default)]
struct SystemdUserScheduler;

impl SystemdUserScheduler {
    async fn list_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        if allowlist_globs.is_empty() {
            return Ok(Vec::new());
        }

        let loaded_output = run_system_command(list_timers_invocation(), COMMAND_TIMEOUT).await?;
        let installed_output =
            run_system_command(list_unit_files_invocation(), COMMAND_TIMEOUT).await?;
        let timer_units = enumerate_timer_units(
            &loaded_output.stdout,
            &installed_output.stdout,
            allowlist_globs,
        );

        let mut summaries = Vec::with_capacity(timer_units.len());
        for unit in timer_units {
            summaries.push(self.summary_for_unit(&unit, allowlist_globs).await?);
        }
        summaries.sort_by(|left, right| left.unit.cmp(&right.unit));
        Ok(summaries)
    }

    async fn job_detail(&self, unit: &str, allowlist_globs: &[String]) -> Result<LocalJobDetail> {
        validate_timer_unit(unit)?;
        ensure_unit_allowlisted(unit, allowlist_globs)?;
        let summary = self.summary_for_unit(unit, allowlist_globs).await?;
        let output = run_system_command(
            journal_tail_invocation(&summary.triggered_unit),
            COMMAND_TIMEOUT,
        )
        .await?;

        Ok(LocalJobDetail {
            summary,
            log_tail: parse_journal_tail(&output.stdout),
        })
    }

    async fn control_job(
        &self,
        unit: &str,
        control: LocalJobControl,
        allowlist_globs: &[String],
    ) -> Result<LocalJobSummary> {
        validate_timer_unit(unit)?;
        ensure_unit_allowlisted(unit, allowlist_globs)?;

        let triggered_unit = if matches!(control, LocalJobControl::Run) {
            let timer_output =
                run_system_command(timer_show_invocation(unit), COMMAND_TIMEOUT).await?;
            parse_triggered_unit(&timer_output.stdout)?
        } else {
            String::new()
        };
        let invocation = build_control_invocation(control, unit, &triggered_unit, allowlist_globs)?;
        run_system_command(invocation, COMMAND_TIMEOUT).await?;

        self.summary_for_unit(unit, allowlist_globs).await
    }

    async fn summary_for_unit(
        &self,
        unit: &str,
        allowlist_globs: &[String],
    ) -> Result<LocalJobSummary> {
        validate_timer_unit(unit)?;
        ensure_unit_allowlisted(unit, allowlist_globs)?;
        let timer_output = run_system_command(timer_show_invocation(unit), COMMAND_TIMEOUT).await?;
        let triggered_unit = parse_triggered_unit(&timer_output.stdout)?;
        let service_output =
            run_system_command(service_show_invocation(&triggered_unit), COMMAND_TIMEOUT).await?;

        parse_local_job_summary(&timer_output.stdout, &service_output.stdout)
    }
}

pub fn build_control_invocation(
    control: LocalJobControl,
    timer_unit: &str,
    triggered_unit: &str,
    allowlist_globs: &[String],
) -> Result<CommandInvocation> {
    validate_timer_unit(timer_unit)?;
    ensure_unit_allowlisted(timer_unit, allowlist_globs)?;

    match control {
        LocalJobControl::Enable => Ok(CommandInvocation {
            program: "systemctl".to_string(),
            args: vec![
                "--user".to_string(),
                "enable".to_string(),
                "--now".to_string(),
                timer_unit.to_string(),
            ],
        }),
        LocalJobControl::Disable => Ok(CommandInvocation {
            program: "systemctl".to_string(),
            args: vec![
                "--user".to_string(),
                "disable".to_string(),
                "--now".to_string(),
                timer_unit.to_string(),
            ],
        }),
        LocalJobControl::Run => {
            validate_service_unit(triggered_unit)?;
            Ok(CommandInvocation {
                program: "systemctl".to_string(),
                args: vec![
                    "--user".to_string(),
                    "start".to_string(),
                    "--no-block".to_string(),
                    triggered_unit.to_string(),
                ],
            })
        }
    }
}

pub fn parse_local_job_summary(timer_show: &str, service_show: &str) -> Result<LocalJobSummary> {
    let timer = parse_key_values(timer_show);
    let service = parse_key_values(service_show);
    let unit = required_value(&timer, "Id")?.to_string();
    validate_timer_unit(&unit)?;
    let triggered_unit = required_value(&timer, "Triggers")?.to_string();
    validate_service_unit(&triggered_unit)?;

    let next_elapse = optional_systemd_timestamp(timer.get("NextElapseUSecRealtime"));
    let last_trigger = optional_systemd_timestamp(timer.get("LastTriggerUSec"));
    let exit_timestamp = optional_systemd_timestamp(service.get("ExecMainExitTimestamp"));
    let exit_code = service
        .get("ExecMainStatus")
        .and_then(|value| value.parse::<i32>().ok());
    let result = service
        .get("Result")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    Ok(LocalJobSummary {
        title: title_for_unit(&unit),
        unit,
        backend: BACKEND_SYSTEMD_USER.to_string(),
        enabled: timer
            .get("UnitFileState")
            .is_some_and(|state| state.starts_with("enabled")),
        active_state: timer
            .get("ActiveState")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        schedule: LocalJobSchedule {
            next_elapse_at: next_elapse,
            interval_hint: None,
            raw: timer
                .get("NextElapseUSecRealtime")
                .cloned()
                .unwrap_or_default(),
        },
        last_fired_at: last_trigger,
        last_exit: LocalJobExit {
            code: exit_code,
            result,
            at: exit_timestamp,
        },
        triggered_unit,
    })
}

pub fn parse_list_timers(stdout: &str) -> Vec<String> {
    let mut units = BTreeSet::new();
    for line in stdout.lines() {
        for token in line.split_whitespace() {
            if token.ends_with(".timer") {
                units.insert(token.to_string());
                break;
            }
        }
    }
    units.into_iter().collect()
}

pub fn parse_list_unit_files(stdout: &str) -> Vec<String> {
    let mut units = BTreeSet::new();
    for line in stdout.lines() {
        let Some(unit) = line.split_whitespace().next() else {
            continue;
        };
        if unit.ends_with(".timer") {
            units.insert(unit.to_string());
        }
    }
    units.into_iter().collect()
}

pub fn enumerate_timer_units(
    loaded_timers_stdout: &str,
    installed_unit_files_stdout: &str,
    allowlist_globs: &[String],
) -> Vec<String> {
    let mut units = BTreeSet::new();
    units.extend(parse_list_timers(loaded_timers_stdout));
    units.extend(parse_list_unit_files(installed_unit_files_stdout));
    units
        .into_iter()
        .filter(|unit| unit_is_allowlisted(unit, allowlist_globs))
        .collect()
}

pub fn parse_journal_tail(stdout: &str) -> Vec<String> {
    stdout.lines().map(ToOwned::to_owned).collect()
}

fn parse_triggered_unit(timer_show: &str) -> Result<String> {
    let timer = parse_key_values(timer_show);
    let triggered_unit = required_value(&timer, "Triggers")?.to_string();
    validate_service_unit(&triggered_unit)?;
    Ok(triggered_unit)
}

fn parse_key_values(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn required_value<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    values
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("systemd output did not include {key}"))
}

fn optional_systemd_timestamp(value: Option<&String>) -> Option<i64> {
    let value = value?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n/a") || value == "0" {
        return None;
    }
    parse_systemd_timestamp(value)
}

fn parse_systemd_timestamp(value: &str) -> Option<i64> {
    if let Some(timestamp) = parse_unix_timestamp(value) {
        return Some(timestamp);
    }

    let mut parts = value.split_whitespace();
    let first = parts.next()?;
    let date = if first.contains('-') {
        first
    } else {
        parts.next()?
    };
    let time = parts.next()?;
    let timezone = parts.next();
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let second = time_parts.next()?.parse::<i64>().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let offset = timezone_offset_seconds(timezone).unwrap_or(0);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

fn parse_unix_timestamp(value: &str) -> Option<i64> {
    let value = value.trim().trim_start_matches('@');
    let whole = value
        .split_once('.')
        .map(|(whole, _)| whole)
        .unwrap_or(value);
    let parsed = whole.parse::<i64>().ok()?;
    if parsed > 10_000_000_000_000 {
        Some(parsed / 1_000_000)
    } else if parsed > 10_000_000_000 {
        Some(parsed / 1_000)
    } else {
        Some(parsed)
    }
}

fn timezone_offset_seconds(timezone: Option<&str>) -> Option<i64> {
    let timezone = timezone?;
    if timezone.eq_ignore_ascii_case("UTC")
        || timezone.eq_ignore_ascii_case("GMT")
        || timezone == "Z"
    {
        return Some(0);
    }
    parse_timezone_offset(timezone).or_else(|| named_timezone_offset_seconds(timezone))
}

fn parse_timezone_offset(value: &str) -> Option<i64> {
    let sign = match value.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &value[1..];
    let (hours, minutes) = if let Some((hours, minutes)) = rest.split_once(':') {
        (hours.parse::<i64>().ok()?, minutes.parse::<i64>().ok()?)
    } else if rest.len() == 4 {
        (
            rest[..2].parse::<i64>().ok()?,
            rest[2..].parse::<i64>().ok()?,
        )
    } else if rest.len() == 2 {
        (rest.parse::<i64>().ok()?, 0)
    } else {
        return None;
    };
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn named_timezone_offset_seconds(timezone: &str) -> Option<i64> {
    match timezone {
        "UTC" | "GMT" => Some(0),
        "EST" => Some(-5 * 3_600),
        "EDT" => Some(-4 * 3_600),
        "CST" => Some(-6 * 3_600),
        "CDT" => Some(-5 * 3_600),
        "MST" => Some(-7 * 3_600),
        "MDT" => Some(-6 * 3_600),
        "PST" => Some(-8 * 3_600),
        "PDT" => Some(-7 * 3_600),
        "CET" => Some(3_600),
        "CEST" => Some(2 * 3_600),
        _ => None,
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn title_for_unit(unit: &str) -> String {
    let stem = unit.strip_suffix(".timer").unwrap_or(unit);
    let words = stem
        .split(['-', '_', '.', '@'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        unit.to_string()
    } else {
        words.join(" ")
    }
}

fn ensure_unit_allowlisted(unit: &str, allowlist_globs: &[String]) -> Result<()> {
    if unit_is_allowlisted(unit, allowlist_globs) {
        Ok(())
    } else {
        bail!("system job unit '{unit}' is not allowlisted")
    }
}

fn unit_is_allowlisted(unit: &str, allowlist_globs: &[String]) -> bool {
    allowlist_globs
        .iter()
        .any(|glob| glob_matches(glob.trim(), unit))
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }

    let pattern = pattern.as_bytes();
    let candidate = candidate.as_bytes();
    let mut table = vec![vec![false; candidate.len() + 1]; pattern.len() + 1];
    table[0][0] = true;

    for index in 1..=pattern.len() {
        if pattern[index - 1] == b'*' {
            table[index][0] = table[index - 1][0];
        }
    }

    for p_index in 1..=pattern.len() {
        for c_index in 1..=candidate.len() {
            table[p_index][c_index] = match pattern[p_index - 1] {
                b'*' => table[p_index - 1][c_index] || table[p_index][c_index - 1],
                b'?' => table[p_index - 1][c_index - 1],
                literal => literal == candidate[c_index - 1] && table[p_index - 1][c_index - 1],
            };
        }
    }

    table[pattern.len()][candidate.len()]
}

fn validate_timer_unit(unit: &str) -> Result<()> {
    validate_unit_name(unit, ".timer")
}

fn validate_service_unit(unit: &str) -> Result<()> {
    validate_unit_name(unit, ".service")
}

fn validate_unit_name(unit: &str, suffix: &str) -> Result<()> {
    if unit.trim().is_empty() || !unit.ends_with(suffix) {
        bail!("systemd unit must end with {suffix}");
    }
    if unit.contains('/') || unit.chars().any(char::is_whitespace) {
        bail!("systemd unit contains unsupported characters");
    }
    Ok(())
}

fn list_timers_invocation() -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "list-timers".to_string(),
            "--all".to_string(),
            "--no-pager".to_string(),
            "--no-legend".to_string(),
        ],
    }
}

fn list_unit_files_invocation() -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "list-unit-files".to_string(),
            "--type=timer".to_string(),
            "--no-pager".to_string(),
            "--no-legend".to_string(),
        ],
    }
}

fn timer_show_invocation(unit: &str) -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "--timestamp=utc".to_string(),
            "show".to_string(),
            unit.to_string(),
            "--property=Id,ActiveState,UnitFileState,LastTriggerUSec,NextElapseUSecRealtime,Triggers"
                .to_string(),
            "--no-pager".to_string(),
        ],
    }
}

fn service_show_invocation(unit: &str) -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "--timestamp=utc".to_string(),
            "show".to_string(),
            unit.to_string(),
            "--property=Result,ExecMainStatus,ExecMainExitTimestamp,ActiveState,SubState"
                .to_string(),
            "--no-pager".to_string(),
        ],
    }
}

fn journal_tail_invocation(unit: &str) -> CommandInvocation {
    CommandInvocation {
        program: "journalctl".to_string(),
        args: vec![
            "--user".to_string(),
            "--user-unit".to_string(),
            unit.to_string(),
            "-n".to_string(),
            "100".to_string(),
            "--no-pager".to_string(),
            "-o".to_string(),
            "short-iso".to_string(),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandOutput {
    stdout: String,
}

async fn run_system_command(
    invocation: CommandInvocation,
    timeout_duration: Duration,
) -> Result<CommandOutput> {
    let started_at = Instant::now();
    let output = Command::new(&invocation.program)
        .args(&invocation.args)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {}", invocation.program))?;

    let output = match timeout(timeout_duration, output.wait_with_output()).await {
        Ok(result) => {
            result.with_context(|| format!("{} failed to execute", invocation.program))?
        }
        Err(_) => bail!(
            "{} {} timed out after {}s",
            invocation.program,
            invocation.args.join(" "),
            started_at.elapsed().as_secs()
        ),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        return Ok(CommandOutput { stdout });
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    bail!(
        "{} {} failed{}",
        invocation.program,
        invocation.args.join(" "),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMER_SHOW: &str = "\
NextElapseUSecRealtime=Sat 2026-06-13 17:30:00 UTC\n\
LastTriggerUSec=Sat 2026-06-13 17:00:01 UTC\n\
Id=placeholder-cleanup.timer\n\
Triggers=placeholder-cleanup.service\n\
ActiveState=active\n\
UnitFileState=enabled\n";

    const SERVICE_SUCCESS_SHOW: &str = "\
Result=success\n\
ExecMainStatus=0\n\
ExecMainExitTimestamp=Sat 2026-06-13 17:00:02 UTC\n\
ActiveState=inactive\n\
SubState=dead\n";

    const SERVICE_FAILED_SHOW: &str = "\
Result=exit-code\n\
ExecMainStatus=2\n\
ExecMainExitTimestamp=Sat 2026-06-13 17:05:04 UTC\n\
ActiveState=failed\n\
SubState=failed\n";

    #[test]
    fn parses_timer_and_successful_service_show_output() {
        let summary = parse_local_job_summary(TIMER_SHOW, SERVICE_SUCCESS_SHOW).unwrap();

        assert_eq!(summary.unit, "placeholder-cleanup.timer");
        assert_eq!(summary.title, "Placeholder Cleanup");
        assert_eq!(summary.backend, "systemd-user");
        assert!(summary.enabled);
        assert_eq!(summary.active_state, "active");
        assert_eq!(summary.triggered_unit, "placeholder-cleanup.service");
        assert_eq!(summary.schedule.next_elapse_at, Some(1_781_371_800));
        assert_eq!(summary.schedule.raw, "Sat 2026-06-13 17:30:00 UTC");
        assert_eq!(summary.last_fired_at, Some(1_781_370_001));
        assert_eq!(summary.last_exit.code, Some(0));
        assert_eq!(summary.last_exit.result, "success");
        assert_eq!(summary.last_exit.at, Some(1_781_370_002));
    }

    #[test]
    fn parses_failed_service_exit_status() {
        let summary = parse_local_job_summary(TIMER_SHOW, SERVICE_FAILED_SHOW).unwrap();

        assert_eq!(summary.last_exit.code, Some(2));
        assert_eq!(summary.last_exit.result, "exit-code");
        assert_eq!(summary.last_exit.at, Some(1_781_370_304));
    }

    #[test]
    fn parses_non_utc_systemd_timestamp_to_correct_epoch() {
        assert_eq!(
            parse_systemd_timestamp("Sat 2026-06-13 10:30:00 -0700"),
            Some(1_781_371_800)
        );
        assert_eq!(
            parse_systemd_timestamp("Sat 2026-06-13 10:30:00 PDT"),
            Some(1_781_371_800)
        );
    }

    #[test]
    fn parses_journal_tail_lines() {
        let lines = parse_journal_tail(
            "2026-06-13T17:00:01+0000 host placeholder[1]: started\n2026-06-13T17:00:02+0000 host placeholder[1]: done\n",
        );

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("started"));
        assert!(lines[1].contains("done"));
    }

    #[test]
    fn parses_list_timers_fixture() {
        let timers = parse_list_timers(
            "Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-cleanup.timer placeholder-cleanup.service\n\
             n/a n/a n/a n/a placeholder-sync.timer placeholder-sync.service\n",
        );

        assert_eq!(
            timers,
            vec![
                "placeholder-cleanup.timer".to_string(),
                "placeholder-sync.timer".to_string()
            ]
        );
    }

    #[test]
    fn enumerates_allowlisted_installed_timer_absent_from_loaded_timers() {
        let units = enumerate_timer_units(
            "Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-loaded.timer placeholder-loaded.service\n",
            "placeholder-installed.timer disabled disabled\nplaceholder-other.timer enabled enabled\n",
            &["placeholder-installed.timer".to_string()],
        );

        assert_eq!(units, vec!["placeholder-installed.timer".to_string()]);
    }

    #[test]
    fn rejects_non_allowlisted_control_before_building_invocation() {
        let result = build_control_invocation(
            LocalJobControl::Disable,
            "placeholder-cleanup.timer",
            "",
            &["placeholder-sync.timer".to_string()],
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not allowlisted"));
    }

    #[test]
    fn enable_and_disable_control_timer_with_now() {
        let allowlist = ["placeholder-cleanup.timer".to_string()];
        let enable = build_control_invocation(
            LocalJobControl::Enable,
            "placeholder-cleanup.timer",
            "",
            &allowlist,
        )
        .unwrap();
        let disable = build_control_invocation(
            LocalJobControl::Disable,
            "placeholder-cleanup.timer",
            "",
            &allowlist,
        )
        .unwrap();

        assert_eq!(
            enable.args,
            vec!["--user", "enable", "--now", "placeholder-cleanup.timer"]
        );
        assert_eq!(
            disable.args,
            vec!["--user", "disable", "--now", "placeholder-cleanup.timer"]
        );
    }

    #[test]
    fn run_now_starts_triggered_service_without_blocking_not_timer() {
        let invocation = build_control_invocation(
            LocalJobControl::Run,
            "placeholder-cleanup.timer",
            "placeholder-cleanup.service",
            &["placeholder-*.timer".to_string()],
        )
        .unwrap();

        assert_eq!(invocation.program, "systemctl");
        assert_eq!(
            invocation.args,
            vec![
                "--user",
                "start",
                "--no-block",
                "placeholder-cleanup.service"
            ]
        );
    }
}
