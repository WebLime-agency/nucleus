use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fmt, fs,
    future::Future,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    pin::Pin,
    process::Command as StdCommand,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use nucleus_protocol::{
    LocalJobDetail, LocalJobExit, LocalJobSchedule, LocalJobSummary,
    SystemJobAuthoringScheduleKind, SystemJobAuthoringSpec, SystemJobRenderedUnits,
    SystemJobTemplate,
};
use tokio::{process::Command as TokioCommand, time::timeout};
use tracing::warn;

const BACKEND_SYSTEMD_USER: &str = "systemd-user";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemJobError {
    NotAllowlisted { unit: String },
    InvalidUnit { reason: String },
    InvalidAuthoringSpec { reason: String },
    UnsupportedTriggeredUnit { unit: String },
    MissingTriggeredUnit { unit: String },
    UnitAlreadyExists { unit: String },
    NotAuthored { unit: String },
}

impl fmt::Display for SystemJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowlisted { unit } => {
                write!(formatter, "system job unit '{unit}' is not allowlisted")
            }
            Self::InvalidUnit { reason } => write!(formatter, "systemd unit {reason}"),
            Self::InvalidAuthoringSpec { reason } => {
                write!(formatter, "system job authoring spec {reason}")
            }
            Self::UnsupportedTriggeredUnit { unit } => {
                write!(
                    formatter,
                    "triggered unit '{unit}' is not supported for this operation"
                )
            }
            Self::MissingTriggeredUnit { unit } => {
                write!(
                    formatter,
                    "system job timer '{unit}' has no triggered service to run"
                )
            }
            Self::UnitAlreadyExists { unit } => {
                write!(formatter, "system job unit '{unit}' already exists")
            }
            Self::NotAuthored { unit } => {
                write!(
                    formatter,
                    "system job unit '{unit}' was not authored by Nucleus"
                )
            }
        }
    }
}

impl std::error::Error for SystemJobError {}

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

type SystemCommandFuture = Pin<Box<dyn Future<Output = Result<CommandOutput>> + Send>>;
type SystemCommandRunner =
    Arc<dyn Fn(CommandInvocation, Duration) -> SystemCommandFuture + Send + Sync>;

#[derive(Debug, Clone, Default)]
pub struct SystemScheduler {
    backend: SystemdUserScheduler,
}

impl SystemScheduler {
    pub fn systemd_user() -> Self {
        Self {
            backend: SystemdUserScheduler::default(),
        }
    }

    pub async fn list_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        self.backend.list_jobs(allowlist_globs).await
    }

    pub async fn available_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        self.backend.available_jobs(allowlist_globs).await
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

    pub fn templates() -> Vec<SystemJobTemplate> {
        authored_job_templates()
    }

    pub fn render_authored(&self, spec: &SystemJobAuthoringSpec) -> Result<SystemJobRenderedUnits> {
        self.backend.render_authored(spec)
    }

    pub async fn install_authored(
        &self,
        spec: &SystemJobAuthoringSpec,
    ) -> Result<SystemJobRenderedUnits> {
        self.backend.install_authored(spec).await
    }

    pub async fn delete_authored(&self, unit: &str) -> Result<()> {
        self.backend.delete_authored(unit).await
    }
}

#[derive(Clone)]
struct SystemdUserScheduler {
    command_runner: SystemCommandRunner,
    unit_dir: Option<PathBuf>,
}

impl fmt::Debug for SystemdUserScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SystemdUserScheduler")
    }
}

impl Default for SystemdUserScheduler {
    fn default() -> Self {
        Self {
            command_runner: Arc::new(|invocation, timeout_duration| {
                Box::pin(run_system_command(invocation, timeout_duration))
            }),
            unit_dir: None,
        }
    }
}

impl SystemdUserScheduler {
    #[cfg(test)]
    fn with_command_runner<F, Fut>(runner: F) -> Self
    where
        F: Fn(CommandInvocation, Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutput>> + Send + 'static,
    {
        Self {
            command_runner: Arc::new(move |invocation, timeout_duration| {
                Box::pin(runner(invocation, timeout_duration))
            }),
            unit_dir: None,
        }
    }

    #[cfg(test)]
    fn with_command_runner_and_unit_dir<F, Fut>(runner: F, unit_dir: PathBuf) -> Self
    where
        F: Fn(CommandInvocation, Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutput>> + Send + 'static,
    {
        Self {
            command_runner: Arc::new(move |invocation, timeout_duration| {
                Box::pin(runner(invocation, timeout_duration))
            }),
            unit_dir: Some(unit_dir),
        }
    }

    async fn run_command(&self, invocation: CommandInvocation) -> Result<CommandOutput> {
        (self.command_runner)(invocation, COMMAND_TIMEOUT).await
    }

    fn render_authored(&self, spec: &SystemJobAuthoringSpec) -> Result<SystemJobRenderedUnits> {
        render_authored_units(spec)
    }

    async fn install_authored(
        &self,
        spec: &SystemJobAuthoringSpec,
    ) -> Result<SystemJobRenderedUnits> {
        let rendered = self.render_authored(spec)?;
        let unit_dir = self.unit_dir()?;
        fs::create_dir_all(&unit_dir).with_context(|| {
            format!(
                "failed to create systemd user unit dir {}",
                unit_dir.display()
            )
        })?;
        let timer_path = unit_dir.join(&rendered.timer_unit);
        let service_path = unit_dir.join(&rendered.service_unit);
        if timer_path.exists() || service_path.exists() {
            return Err(SystemJobError::UnitAlreadyExists {
                unit: rendered.timer_unit.clone(),
            }
            .into());
        }

        write_new_unit_file(&timer_path, &rendered.timer)?;
        if let Err(error) = write_new_unit_file(&service_path, &rendered.service) {
            let _ = fs::remove_file(&timer_path);
            return Err(error);
        }

        if let Err(error) = self.run_command(daemon_reload_invocation()).await {
            let _ = fs::remove_file(&timer_path);
            let _ = fs::remove_file(&service_path);
            return Err(error);
        }
        if let Err(error) = self
            .run_command(enable_timer_invocation(&rendered.timer_unit))
            .await
        {
            let _ = fs::remove_file(&timer_path);
            let _ = fs::remove_file(&service_path);
            let _ = self.run_command(daemon_reload_invocation()).await;
            return Err(error);
        }

        Ok(rendered)
    }

    async fn delete_authored(&self, unit: &str) -> Result<()> {
        validate_timer_unit(unit)?;
        self.run_command(disable_timer_invocation(unit)).await?;
        let unit_dir = self.unit_dir()?;
        let service_unit = service_unit_for_timer(unit)?;
        remove_unit_file_if_exists(unit_dir.join(unit))?;
        remove_unit_file_if_exists(unit_dir.join(service_unit))?;
        self.run_command(daemon_reload_invocation()).await?;
        Ok(())
    }

    fn unit_dir(&self) -> Result<PathBuf> {
        if let Some(unit_dir) = &self.unit_dir {
            return Ok(unit_dir.clone());
        }
        default_systemd_user_unit_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            dirs::home_dir(),
        )
    }

    async fn list_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        if allowlist_globs.is_empty() {
            return Ok(Vec::new());
        }

        let loaded_output = self.run_command(list_timers_invocation()).await?;
        let installed_output = self.run_command(list_unit_files_invocation()).await?;
        let listed_next_elapses = parse_list_timer_next_elapses(&loaded_output.stdout);
        let timer_units = enumerate_timer_units(
            &loaded_output.stdout,
            &installed_output.stdout,
            allowlist_globs,
        );

        self.summaries_for_units(timer_units, allowlist_globs, &listed_next_elapses)
            .await
    }

    async fn available_jobs(&self, allowlist_globs: &[String]) -> Result<Vec<LocalJobSummary>> {
        let loaded_output = self.run_command(list_timers_invocation()).await?;
        let installed_output = self.run_command(list_unit_files_invocation()).await?;
        let listed_next_elapses = parse_list_timer_next_elapses(&loaded_output.stdout);
        let timer_units =
            enumerate_timer_units_unfiltered(&loaded_output.stdout, &installed_output.stdout);

        self.summaries_for_units(timer_units, allowlist_globs, &listed_next_elapses)
            .await
    }

    async fn summaries_for_units(
        &self,
        timer_units: Vec<String>,
        allowlist_globs: &[String],
        listed_next_elapses: &BTreeMap<String, String>,
    ) -> Result<Vec<LocalJobSummary>> {
        let mut summaries = Vec::with_capacity(timer_units.len());
        for unit in timer_units {
            match self
                .summary_for_unit_unchecked_with_listed_next(
                    &unit,
                    allowlist_globs,
                    listed_next_elapses.get(&unit).map(String::as_str),
                )
                .await
            {
                Ok(summary) => summaries.push(summary),
                Err(error) => {
                    warn!(unit = %unit, error = %error, "failed to inspect local system job; skipping unit");
                }
            }
        }
        summaries.sort_by(|left, right| left.unit.cmp(&right.unit));
        Ok(summaries)
    }

    async fn job_detail(&self, unit: &str, allowlist_globs: &[String]) -> Result<LocalJobDetail> {
        validate_timer_unit(unit)?;
        ensure_unit_allowlisted(unit, allowlist_globs)?;
        let summary = self.summary_for_unit(unit, allowlist_globs).await?;
        if summary.triggered_unit.is_empty() {
            return Ok(LocalJobDetail {
                summary,
                log_tail: Vec::new(),
            });
        }

        let output = self
            .run_command(journal_tail_invocation(&summary.triggered_unit))
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
            let timer_output = self.run_command(timer_show_invocation(unit)).await?;
            parse_triggered_unit(&timer_output.stdout, unit)?
        } else {
            String::new()
        };
        let invocation = build_control_invocation(control, unit, &triggered_unit, allowlist_globs)?;
        self.run_command(invocation).await?;

        self.summary_for_unit(unit, allowlist_globs).await
    }

    async fn summary_for_unit(
        &self,
        unit: &str,
        allowlist_globs: &[String],
    ) -> Result<LocalJobSummary> {
        let listed_next_elapse = self
            .run_command(list_timers_invocation())
            .await
            .ok()
            .and_then(|output| parse_list_timer_next_elapses(&output.stdout).remove(unit));
        validate_timer_unit(unit)?;
        ensure_unit_allowlisted(unit, allowlist_globs)?;
        self.summary_for_unit_unchecked_with_listed_next(
            unit,
            allowlist_globs,
            listed_next_elapse.as_deref(),
        )
        .await
    }

    async fn summary_for_unit_unchecked_with_listed_next(
        &self,
        unit: &str,
        allowlist_globs: &[String],
        listed_next_elapse: Option<&str>,
    ) -> Result<LocalJobSummary> {
        validate_timer_unit(unit)?;
        let timer_output = self.run_command(timer_show_invocation(unit)).await?;
        let timer = parse_key_values(&timer_output.stdout);
        let triggered_unit = optional_triggered_unit(&timer).unwrap_or_default();
        if !triggered_unit.is_empty() {
            validate_triggered_unit(&triggered_unit)?;
        }
        let service_stdout = if triggered_unit.ends_with(".service") {
            self.run_command(service_show_invocation(&triggered_unit))
                .await?
                .stdout
        } else {
            String::new()
        };

        let mut summary = parse_local_job_summary_with_listed_next(
            &timer_output.stdout,
            &service_stdout,
            listed_next_elapse,
        )?;
        summary.managed = unit_is_allowlisted(&summary.unit, allowlist_globs);
        Ok(summary)
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
            if triggered_unit.trim().is_empty() {
                return Err(SystemJobError::MissingTriggeredUnit {
                    unit: timer_unit.to_string(),
                }
                .into());
            }
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

pub fn render_authored_units(spec: &SystemJobAuthoringSpec) -> Result<SystemJobRenderedUnits> {
    let timer_unit = normalize_authored_timer_name(&spec.name)?;
    let service_unit = service_unit_for_timer(&timer_unit)?;
    let description = validate_optional_unit_text(spec.description.as_deref(), "description")?
        .unwrap_or_else(|| title_for_unit(&timer_unit));
    let argv = parse_direct_command(&spec.command)?;
    validate_direct_command(&argv)?;
    let exec_start = argv
        .iter()
        .map(|arg| quote_systemd_exec_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let schedule_line = match spec.schedule.kind {
        SystemJobAuthoringScheduleKind::Interval => {
            let value = validate_schedule_value(
                &spec.schedule.value,
                "interval",
                SystemJobAuthoringScheduleKind::Interval,
            )?;
            let value = escape_systemd_value(&value);
            format!("OnActiveSec={value}\nOnUnitActiveSec={value}")
        }
        SystemJobAuthoringScheduleKind::Calendar => {
            let value = validate_schedule_value(
                &spec.schedule.value,
                "calendar",
                SystemJobAuthoringScheduleKind::Calendar,
            )?;
            format!("OnCalendar={}", escape_systemd_value(&value))
        }
    };
    let working_dir = spec
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(validate_working_dir)
        .transpose()?;
    let working_dir_line = working_dir
        .as_ref()
        .map(|dir| format!("WorkingDirectory={}\n", escape_systemd_value(dir)))
        .unwrap_or_default();
    let description = escape_systemd_value(&description);
    let timer = format!(
        "# Nucleus-authored systemd user timer. Preview before installing; safe to delete through Nucleus.\n\
[Unit]\n\
Description={description}\n\
\n\
[Timer]\n\
{schedule_line}\n\
Unit={service_unit}\n\
Persistent=true\n\
\n\
[Install]\n\
WantedBy=timers.target\n"
    );
    let service = format!(
        "# Nucleus-authored systemd user service. ExecStart runs the authored command directly.\n\
[Unit]\n\
Description={description}\n\
\n\
[Service]\n\
Type=oneshot\n\
{working_dir_line}\
ExecStart={exec_start}\n"
    );

    Ok(SystemJobRenderedUnits {
        name: timer_unit.clone(),
        timer_unit,
        service_unit,
        timer,
        service,
    })
}

fn authored_job_templates() -> Vec<SystemJobTemplate> {
    vec![
        SystemJobTemplate {
            id: "interval-command".to_string(),
            title: "Run a command on an interval".to_string(),
            summary: "A systemd user timer that runs a direct command repeatedly.".to_string(),
            spec: SystemJobAuthoringSpec {
                name: "placeholder-interval.timer".to_string(),
                description: Some("Run placeholder interval command".to_string()),
                command: "/usr/bin/printf interval".to_string(),
                schedule: nucleus_protocol::SystemJobAuthoringSchedule {
                    kind: SystemJobAuthoringScheduleKind::Interval,
                    value: "30min".to_string(),
                },
                working_dir: None,
            },
        },
        SystemJobTemplate {
            id: "calendar-command".to_string(),
            title: "Run a command on a calendar schedule".to_string(),
            summary: "A systemd user timer that runs at a calendar time.".to_string(),
            spec: SystemJobAuthoringSpec {
                name: "placeholder-calendar.timer".to_string(),
                description: Some("Run placeholder calendar command".to_string()),
                command: "/usr/bin/printf calendar".to_string(),
                schedule: nucleus_protocol::SystemJobAuthoringSchedule {
                    kind: SystemJobAuthoringScheduleKind::Calendar,
                    value: "*-*-* 04:00:00".to_string(),
                },
                working_dir: None,
            },
        },
        SystemJobTemplate {
            id: "custom-command".to_string(),
            title: "Custom systemd user timer".to_string(),
            summary: "Start from a blank direct command and choose the schedule.".to_string(),
            spec: SystemJobAuthoringSpec {
                name: "placeholder-custom.timer".to_string(),
                description: None,
                command: "/usr/bin/printf custom".to_string(),
                schedule: nucleus_protocol::SystemJobAuthoringSchedule {
                    kind: SystemJobAuthoringScheduleKind::Interval,
                    value: "1h".to_string(),
                },
                working_dir: None,
            },
        },
    ]
}

fn normalize_authored_timer_name(name: &str) -> Result<String> {
    let name = name.trim();
    validate_timer_unit(name)?;
    let stem = name.strip_suffix(".timer").unwrap_or(name);
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason:
                "name may contain only letters, numbers, dash, underscore, and the .timer suffix"
                    .to_string(),
        }
        .into());
    }
    Ok(name.to_string())
}

fn service_unit_for_timer(timer_unit: &str) -> Result<String> {
    let stem = timer_unit
        .strip_suffix(".timer")
        .ok_or_else(|| SystemJobError::InvalidUnit {
            reason: "must end with .timer".to_string(),
        })?;
    Ok(format!("{stem}.service"))
}

fn validate_optional_unit_text(value: Option<&str>, field: &str) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 240 || value.chars().any(|ch| ch.is_control()) {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: format!("{field} contains unsupported characters"),
        }
        .into());
    }
    Ok(Some(value.to_string()))
}

fn validate_schedule_value(
    value: &str,
    label: &str,
    kind: SystemJobAuthoringScheduleKind,
) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 160
        || value.starts_with('-')
        || value.chars().any(|ch| ch.is_control() || ch == ';')
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: format!("{label} schedule contains unsupported characters"),
        }
        .into());
    }
    validate_systemd_schedule(value, label, kind)?;
    Ok(value.to_string())
}

fn validate_systemd_schedule(
    value: &str,
    label: &str,
    kind: SystemJobAuthoringScheduleKind,
) -> Result<()> {
    match systemd_analyze_schedule(value, &kind) {
        ScheduleProbe::Valid => Ok(()),
        ScheduleProbe::Invalid => Err(SystemJobError::InvalidAuthoringSpec {
            reason: format!("{label} schedule is not a valid systemd expression"),
        }
        .into()),
        ScheduleProbe::Unavailable => {
            let valid = match kind {
                SystemJobAuthoringScheduleKind::Interval => fallback_interval_schedule(value),
                SystemJobAuthoringScheduleKind::Calendar => fallback_calendar_schedule(value),
            };
            if valid {
                Ok(())
            } else {
                Err(SystemJobError::InvalidAuthoringSpec {
                    reason: format!(
                        "{label} schedule does not look like a valid systemd expression"
                    ),
                }
                .into())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleProbe {
    Valid,
    Invalid,
    Unavailable,
}

fn systemd_analyze_schedule(value: &str, kind: &SystemJobAuthoringScheduleKind) -> ScheduleProbe {
    let subcommand = match kind {
        SystemJobAuthoringScheduleKind::Interval => "timespan",
        SystemJobAuthoringScheduleKind::Calendar => "calendar",
    };
    match StdCommand::new("systemd-analyze")
        .arg(subcommand)
        .arg(value)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => ScheduleProbe::Valid,
        Ok(_) => ScheduleProbe::Invalid,
        Err(error) if error.kind() == ErrorKind::NotFound => ScheduleProbe::Unavailable,
        Err(_) => ScheduleProbe::Unavailable,
    }
}

fn fallback_interval_schedule(value: &str) -> bool {
    let compact = value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let mut chars = compact.char_indices().peekable();
    let mut saw_component = false;
    while chars.peek().is_some() {
        let number_start = chars.peek().map(|(index, _)| *index).unwrap_or(0);
        while matches!(chars.peek(), Some((_, ch)) if ch.is_ascii_digit()) {
            chars.next();
        }
        let number_end = chars
            .peek()
            .map(|(index, _)| *index)
            .unwrap_or(compact.len());
        if number_start == number_end {
            return false;
        }
        let unit_start = number_end;
        while matches!(chars.peek(), Some((_, ch)) if ch.is_ascii_alphabetic()) {
            chars.next();
        }
        let unit_end = chars
            .peek()
            .map(|(index, _)| *index)
            .unwrap_or(compact.len());
        if unit_start == unit_end {
            return false;
        }
        let number = &compact[number_start..number_end];
        let unit = compact[unit_start..unit_end].to_ascii_lowercase();
        if number
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .is_none()
            || !matches!(
                unit.as_str(),
                "usec"
                    | "us"
                    | "ms"
                    | "millisecond"
                    | "milliseconds"
                    | "s"
                    | "sec"
                    | "second"
                    | "seconds"
                    | "m"
                    | "min"
                    | "minute"
                    | "minutes"
                    | "h"
                    | "hr"
                    | "hour"
                    | "hours"
                    | "d"
                    | "day"
                    | "days"
                    | "w"
                    | "week"
                    | "weeks"
                    | "month"
                    | "months"
                    | "y"
                    | "year"
                    | "years"
            )
        {
            return false;
        }
        saw_component = true;
    }
    saw_component
}

fn fallback_calendar_schedule(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "minutely"
            | "hourly"
            | "daily"
            | "weekly"
            | "monthly"
            | "quarterly"
            | "semiannually"
            | "annually"
            | "yearly"
    ) {
        return true;
    }
    value.chars().any(|ch| ch.is_ascii_digit())
        && value.contains(':')
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch.is_ascii_whitespace()
                || matches!(ch, '*' | '-' | '_' | ':' | ',' | '/' | '.' | '~')
        })
}

fn validate_working_dir(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 4096
        || value.contains('\0')
        || value.chars().any(|ch| ch == '\n' || ch == '\r')
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "working_dir contains unsupported characters".to_string(),
        }
        .into());
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "working_dir must be an absolute path without parent-directory traversal"
                .to_string(),
        }
        .into());
    }
    Ok(value.to_string())
}

fn parse_direct_command(command: &str) -> Result<Vec<String>> {
    let command = command.trim();
    if command.is_empty()
        || command
            .chars()
            .any(|ch| ch == '\0' || ch == '\n' || ch == '\r')
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command must be a non-empty single-line direct command".to_string(),
        }
        .into());
    }

    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut token_started = false;

    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, ch) if ch.is_whitespace() => {
                if token_started {
                    args.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                token_started = true;
            }
            (Some(active), ch) if ch == active => {
                quote = None;
                token_started = true;
            }
            (_, '\\') => {
                let Some(next) = chars.next() else {
                    return Err(SystemJobError::InvalidAuthoringSpec {
                        reason: "command contains a trailing escape".to_string(),
                    }
                    .into());
                };
                current.push(next);
                token_started = true;
            }
            _ => {
                current.push(ch);
                token_started = true;
            }
        }
    }

    if quote.is_some() {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command contains an unterminated quote".to_string(),
        }
        .into());
    }
    if token_started {
        args.push(current);
    }
    if args.is_empty() {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command must include an executable".to_string(),
        }
        .into());
    }
    Ok(args)
}

fn validate_direct_command(argv: &[String]) -> Result<()> {
    let executable = argv.first().map(String::as_str).unwrap_or_default();
    if executable.trim().is_empty()
        || executable.starts_with('-')
        || executable.contains('/') && executable.ends_with('/')
    {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command must include an executable".to_string(),
        }
        .into());
    }
    if !Path::new(executable).is_absolute() {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command executable must be an absolute path".to_string(),
        }
        .into());
    }
    let basename = executable.rsplit('/').next().unwrap_or(executable);
    if matches!(basename, "nucleus" | "nucleus-daemon") {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command may not call back into the Nucleus daemon".to_string(),
        }
        .into());
    }
    if matches!(basename, "sh" | "bash" | "dash" | "zsh" | "fish" | "env") {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command may not use a shell or env wrapper".to_string(),
        }
        .into());
    }
    if argv.iter().any(|arg| arg.chars().any(char::is_control)) {
        return Err(SystemJobError::InvalidAuthoringSpec {
            reason: "command contains unsupported characters".to_string(),
        }
        .into());
    }
    Ok(())
}

fn quote_systemd_exec_arg(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for ch in value.chars() {
        match ch {
            '\\' | '"' => {
                output.push('\\');
                output.push(ch);
            }
            '%' => output.push_str("%%"),
            '$' => output.push_str("$$"),
            _ => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn escape_systemd_value(value: &str) -> String {
    value.replace('%', "%%")
}

fn write_new_unit_file(path: &Path, content: &str) -> Result<()> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(content.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            Err(SystemJobError::UnitAlreadyExists {
                unit: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("systemd unit")
                    .to_string(),
            }
            .into())
        }
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn remove_unit_file_if_exists(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn default_systemd_user_unit_dir(
    xdg_config_home: Option<&OsStr>,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(value) = xdg_config_home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join("systemd/user"));
    }
    let home = home_dir.context("failed to resolve home directory")?;
    Ok(home.join(".config/systemd/user"))
}

pub fn parse_local_job_summary(timer_show: &str, service_show: &str) -> Result<LocalJobSummary> {
    parse_local_job_summary_with_listed_next(timer_show, service_show, None)
}

fn parse_local_job_summary_with_listed_next(
    timer_show: &str,
    service_show: &str,
    listed_next_elapse: Option<&str>,
) -> Result<LocalJobSummary> {
    let timer = parse_key_values(timer_show);
    let service = parse_key_values(service_show);
    let unit = required_value(&timer, "Id")?.to_string();
    validate_timer_unit(&unit)?;
    let triggered_unit = optional_triggered_unit(&timer).unwrap_or_default();
    if !triggered_unit.is_empty() {
        validate_triggered_unit(&triggered_unit)?;
    }

    let next_elapse_realtime = timer.get("NextElapseUSecRealtime");
    let next_elapse_monotonic = timer.get("NextElapseUSecMonotonic");
    let next_elapse = optional_systemd_timestamp(next_elapse_realtime)
        .or_else(|| listed_next_elapse.and_then(parse_optional_systemd_timestamp));
    let exit_timestamp = optional_systemd_timestamp(service.get("ExecMainExitTimestamp"));
    let last_trigger = optional_systemd_timestamp(timer.get("LastTriggerUSec")).or(exit_timestamp);
    let unit_file_state = timer
        .get("UnitFileState")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
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
        managed: false,
        authored: false,
        enabled: unit_file_state.starts_with("enabled"),
        manageable: unit_file_state_is_manageable(&unit_file_state),
        unit_file_state,
        active_state: timer
            .get("ActiveState")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        schedule: LocalJobSchedule {
            next_elapse_at: next_elapse,
            interval_hint: None,
            raw: next_elapse_realtime
                .and_then(|value| non_empty_str(value.as_str()))
                .or_else(|| listed_next_elapse.and_then(non_empty_str))
                .or_else(|| next_elapse_monotonic.and_then(|value| non_empty_str(value.as_str())))
                .unwrap_or_default()
                .to_string(),
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

fn unit_file_state_is_manageable(state: &str) -> bool {
    matches!(state, "enabled" | "enabled-runtime" | "disabled")
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n/a") || value == "0" {
        None
    } else {
        Some(value)
    }
}

pub fn parse_list_timers(stdout: &str) -> Vec<String> {
    parse_list_timer_next_elapses(stdout).into_keys().collect()
}

pub fn parse_list_timer_next_elapses(stdout: &str) -> BTreeMap<String, String> {
    let mut units = BTreeSet::new();
    let mut next_elapses = BTreeMap::new();
    for line in stdout.lines() {
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        for (index, token) in tokens.iter().enumerate() {
            if token.ends_with(".timer") {
                let unit = token.to_string();
                units.insert(unit.clone());
                if let Some(next_elapse) = parse_list_timer_next_elapse(&tokens[..index]) {
                    next_elapses.insert(unit, next_elapse);
                }
                break;
            }
        }
    }
    for unit in units {
        next_elapses.entry(unit).or_default();
    }
    next_elapses
}

fn parse_list_timer_next_elapse(tokens_before_unit: &[&str]) -> Option<String> {
    if tokens_before_unit.is_empty() || tokens_before_unit[0].eq_ignore_ascii_case("n/a") {
        return None;
    }

    let candidates = if tokens_before_unit[0].contains('-') {
        [3_usize, 0]
    } else {
        [4_usize, 3]
    };

    for len in candidates.into_iter().filter(|len| *len > 0) {
        if tokens_before_unit.len() < len {
            continue;
        }
        let raw = tokens_before_unit[..len].join(" ");
        if parse_systemd_timestamp(&raw).is_some() {
            return Some(raw);
        }
    }

    None
}

pub fn parse_list_unit_files(stdout: &str) -> Vec<String> {
    let mut units = BTreeSet::new();
    for line in stdout.lines() {
        let Some(unit) = line.split_whitespace().next() else {
            continue;
        };
        if unit.ends_with(".timer") && !is_uninstantiated_timer_template(unit) {
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
    enumerate_timer_units_unfiltered(loaded_timers_stdout, installed_unit_files_stdout)
        .into_iter()
        .filter(|unit| unit_is_allowlisted(unit, allowlist_globs))
        .collect()
}

pub fn enumerate_timer_units_unfiltered(
    loaded_timers_stdout: &str,
    installed_unit_files_stdout: &str,
) -> Vec<String> {
    let mut units = BTreeSet::new();
    units.extend(parse_list_timers(loaded_timers_stdout));
    units.extend(parse_list_unit_files(installed_unit_files_stdout));
    units
        .into_iter()
        .filter(|unit| !is_uninstantiated_timer_template(unit))
        .collect()
}

pub fn parse_journal_tail(stdout: &str) -> Vec<String> {
    stdout.lines().map(ToOwned::to_owned).collect()
}

fn parse_triggered_unit(timer_show: &str, unit: &str) -> Result<String> {
    let timer = parse_key_values(timer_show);
    let triggered_unit =
        optional_triggered_unit(&timer).ok_or_else(|| SystemJobError::MissingTriggeredUnit {
            unit: unit.to_string(),
        })?;
    validate_triggered_service_unit(&triggered_unit)?;
    Ok(triggered_unit)
}

fn optional_triggered_unit(values: &BTreeMap<String, String>) -> Option<String> {
    values
        .get("Triggers")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    parse_optional_systemd_timestamp(value)
}

fn parse_optional_systemd_timestamp(value: &str) -> Option<i64> {
    let value = value.trim();
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
        Err(SystemJobError::NotAllowlisted {
            unit: unit.to_string(),
        }
        .into())
    }
}

pub(crate) fn unit_is_allowlisted(unit: &str, allowlist_globs: &[String]) -> bool {
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

fn validate_triggered_unit(unit: &str) -> Result<()> {
    if unit.ends_with(".timer") {
        return Err(SystemJobError::UnsupportedTriggeredUnit {
            unit: unit.to_string(),
        }
        .into());
    }
    let suffix = unit_suffix(unit).ok_or_else(|| SystemJobError::InvalidUnit {
        reason: "must include a unit suffix".to_string(),
    })?;
    validate_unit_name(unit, suffix)
}

fn validate_triggered_service_unit(unit: &str) -> Result<()> {
    if !unit.ends_with(".service") {
        return Err(SystemJobError::UnsupportedTriggeredUnit {
            unit: unit.to_string(),
        }
        .into());
    }
    validate_service_unit(unit)
}

fn unit_suffix(unit: &str) -> Option<&str> {
    let suffix_index = unit.rfind('.')?;
    Some(&unit[suffix_index..])
}

fn validate_unit_name(unit: &str, suffix: &str) -> Result<()> {
    if unit.trim().is_empty() || !unit.ends_with(suffix) {
        return Err(SystemJobError::InvalidUnit {
            reason: format!("must end with {suffix}"),
        }
        .into());
    }
    if unit.starts_with('-') {
        return Err(SystemJobError::InvalidUnit {
            reason: "contains unsupported characters".to_string(),
        }
        .into());
    }
    if unit.contains('/') || unit.chars().any(char::is_whitespace) {
        return Err(SystemJobError::InvalidUnit {
            reason: "contains unsupported characters".to_string(),
        }
        .into());
    }
    Ok(())
}

fn is_uninstantiated_timer_template(unit: &str) -> bool {
    unit.ends_with("@.timer")
}

fn list_timers_invocation() -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "--timestamp=utc".to_string(),
            "list-timers".to_string(),
            "--all".to_string(),
            "--full".to_string(),
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
            "--full".to_string(),
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
            "--property=Id,ActiveState,UnitFileState,LastTriggerUSec,NextElapseUSecRealtime,NextElapseUSecMonotonic,Triggers"
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

fn daemon_reload_invocation() -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec!["--user".to_string(), "daemon-reload".to_string()],
    }
}

fn enable_timer_invocation(unit: &str) -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            unit.to_string(),
        ],
    }
}

fn disable_timer_invocation(unit: &str) -> CommandInvocation {
    CommandInvocation {
        program: "systemctl".to_string(),
        args: vec![
            "--user".to_string(),
            "disable".to_string(),
            "--now".to_string(),
            unit.to_string(),
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
    let output = TokioCommand::new(&invocation.program)
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
    use std::{
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

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

    const FAILED_TIMER_WITHOUT_TRIGGER_SHOW: &str = "\
NextElapseUSecRealtime=Sat 2026-06-13 17:30:00 UTC\n\
LastTriggerUSec=Sat 2026-06-13 17:00:01 UTC\n\
Id=placeholder-failed.timer\n\
Triggers=\n\
ActiveState=failed\n\
UnitFileState=enabled\n";

    const LIST_TIMERS_WITH_FAILED_TIMER: &str = "\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-cleanup.timer placeholder-cleanup.service\n\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-failed.timer\n";

    const LIST_UNIT_FILES_WITH_FAILED_TIMER: &str = "\
placeholder-cleanup.timer enabled enabled\n\
placeholder-failed.timer enabled enabled\n";

    const LIST_TIMERS_WITH_AVAILABLE_TIMERS: &str = "\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-cleanup.timer placeholder-cleanup.service\n\
n/a n/a n/a n/a placeholder-discovered.timer placeholder-discovered.service\n\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-failed.timer\n";

    const LIST_UNIT_FILES_WITH_AVAILABLE_TIMERS: &str = "\
placeholder-cleanup.timer enabled enabled\n\
placeholder-discovered.timer disabled disabled\n\
placeholder-failed.timer enabled enabled\n\
placeholder@.timer disabled disabled\n";

    const DISCOVERED_TIMER_SHOW: &str = "\
NextElapseUSecRealtime=n/a\n\
NextElapseUSecMonotonic=123456789\n\
LastTriggerUSec=n/a\n\
Id=placeholder-discovered.timer\n\
Triggers=placeholder-discovered.service\n\
ActiveState=inactive\n\
UnitFileState=disabled\n";

    const TARGET_TIMER_SHOW: &str = "\
NextElapseUSecRealtime=n/a\n\
NextElapseUSecMonotonic=123456789\n\
LastTriggerUSec=n/a\n\
Id=placeholder-target.timer\n\
Triggers=placeholder.target\n\
ActiveState=inactive\n\
UnitFileState=disabled\n";

    const LIST_TIMERS_WITH_BROKEN_TIMER: &str = "\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-cleanup.timer placeholder-cleanup.service\n\
Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-broken.timer placeholder-broken.service\n";

    const LIST_UNIT_FILES_WITH_BROKEN_TIMER: &str = "\
placeholder-cleanup.timer enabled enabled\n\
placeholder-broken.timer enabled enabled\n";

    fn authored_spec() -> SystemJobAuthoringSpec {
        SystemJobAuthoringSpec {
            name: "placeholder-sync.timer".to_string(),
            description: Some("Placeholder sync".to_string()),
            command: "/usr/bin/printf 'hello world' \"$HOME\" 100%".to_string(),
            schedule: nucleus_protocol::SystemJobAuthoringSchedule {
                kind: SystemJobAuthoringScheduleKind::Interval,
                value: "30min".to_string(),
            },
            working_dir: Some("/tmp/placeholder workspace".to_string()),
        }
    }

    fn test_unit_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "nucleus-system-jobs-{label}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn default_unit_dir_honors_xdg_config_home() {
        let unit_dir = default_systemd_user_unit_dir(
            Some(OsStr::new("/tmp/nucleus-xdg-config")),
            Some(PathBuf::from("/home/example")),
        )
        .unwrap();

        assert_eq!(
            unit_dir,
            PathBuf::from("/tmp/nucleus-xdg-config/systemd/user")
        );

        let fallback =
            default_systemd_user_unit_dir(None, Some(PathBuf::from("/home/example"))).unwrap();
        assert_eq!(
            fallback,
            PathBuf::from("/home/example/.config/systemd/user")
        );
    }

    fn assert_invalid_authoring_reason(error: anyhow::Error, expected: &str) {
        match error.downcast_ref::<SystemJobError>() {
            Some(SystemJobError::InvalidAuthoringSpec { reason }) => {
                assert!(
                    reason.contains(expected),
                    "expected error reason to contain {expected:?}, got {reason:?}"
                );
            }
            other => panic!("expected invalid authoring spec error, got {other:?}"),
        }
    }

    #[test]
    fn renders_authored_interval_units_with_direct_escaped_exec_start() {
        let rendered = render_authored_units(&authored_spec()).unwrap();

        assert_eq!(rendered.timer_unit, "placeholder-sync.timer");
        assert_eq!(rendered.service_unit, "placeholder-sync.service");
        assert!(rendered.timer.contains("# Nucleus-authored"));
        assert!(rendered.timer.contains("OnActiveSec=30min"));
        assert!(rendered.timer.contains("OnUnitActiveSec=30min"));
        assert!(rendered.timer.contains("Unit=placeholder-sync.service"));
        assert!(rendered.service.contains("# Nucleus-authored"));
        assert!(rendered.service.contains("Type=oneshot"));
        assert!(
            rendered
                .service
                .contains("ExecStart=\"/usr/bin/printf\" \"hello world\" \"$$HOME\" \"100%%\"")
        );
        assert!(
            rendered
                .service
                .contains("WorkingDirectory=/tmp/placeholder workspace")
        );
        assert!(!rendered.service.contains("sh -c"));
        assert!(!rendered.service.contains("nucleus run"));
    }

    #[test]
    fn rejects_authored_relative_or_bare_executable() {
        let mut spec = authored_spec();
        spec.command = "python /tmp/example.py".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "executable must be an absolute path");

        spec = authored_spec();
        spec.command = "./run.sh".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "executable must be an absolute path");

        spec = authored_spec();
        spec.command = "/usr/bin/printf absolute".to_string();
        let rendered = render_authored_units(&spec).unwrap();
        assert!(
            rendered
                .service
                .contains("ExecStart=\"/usr/bin/printf\" \"absolute\"")
        );
    }

    #[test]
    fn renders_authored_calendar_units() {
        let mut spec = authored_spec();
        spec.schedule.kind = SystemJobAuthoringScheduleKind::Calendar;
        spec.schedule.value = "*-*-* 04:00:00".to_string();

        let rendered = render_authored_units(&spec).unwrap();

        assert!(rendered.timer.contains("OnCalendar=*-*-* 04:00:00"));
        assert!(!rendered.timer.contains("OnActiveSec="));
        assert!(!rendered.timer.contains("OnUnitActiveSec="));
    }

    #[test]
    fn rejects_authored_name_traversal_and_daemon_callbacks() {
        let mut spec = authored_spec();
        spec.name = "../placeholder.timer".to_string();
        assert!(render_authored_units(&spec).is_err());

        spec = authored_spec();
        spec.command = "/opt/nucleus/bin/nucleus run-placeholder".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "may not call back");

        spec = authored_spec();
        spec.command = "/bin/sh -c 'nucleus run-placeholder'".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "shell or env wrapper");

        spec = authored_spec();
        spec.command = "/usr/bin/env nucleus run-placeholder".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "shell or env wrapper");

        spec = authored_spec();
        spec.command = "/usr/bin/printf 'unterminated".to_string();
        assert!(render_authored_units(&spec).is_err());
    }

    #[test]
    fn rejects_authored_invalid_schedule() {
        let mut spec = authored_spec();
        spec.schedule.value = "definitely-not-a-timespan".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "schedule");

        spec = authored_spec();
        spec.schedule.kind = SystemJobAuthoringScheduleKind::Calendar;
        spec.schedule.value = "not a calendar".to_string();
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "schedule");
    }

    #[test]
    fn rejects_authored_invalid_working_dir() {
        let mut spec = authored_spec();
        spec.working_dir = Some("relative/path".to_string());
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "working_dir must be an absolute path");

        spec = authored_spec();
        spec.working_dir = Some("/tmp/../workspace".to_string());
        let error = render_authored_units(&spec).unwrap_err();
        assert_invalid_authoring_reason(error, "without parent-directory traversal");
    }

    #[tokio::test]
    async fn install_authored_writes_units_and_runs_reload_enable() {
        let unit_dir = test_unit_dir("install");
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let recorded_invocations = Arc::clone(&invocations);
        let scheduler = SystemdUserScheduler::with_command_runner_and_unit_dir(
            move |invocation, _timeout| {
                recorded_invocations
                    .lock()
                    .unwrap()
                    .push(invocation.clone());
                async move {
                    Ok(CommandOutput {
                        stdout: String::new(),
                    })
                }
            },
            unit_dir.clone(),
        );

        let rendered = scheduler.install_authored(&authored_spec()).await.unwrap();

        assert_eq!(rendered.timer_unit, "placeholder-sync.timer");
        assert!(unit_dir.join("placeholder-sync.timer").exists());
        assert!(unit_dir.join("placeholder-sync.service").exists());
        let invocations = invocations.lock().unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].args, vec!["--user", "daemon-reload"]);
        assert_eq!(
            invocations[1].args,
            vec!["--user", "enable", "--now", "placeholder-sync.timer"]
        );

        let _ = fs::remove_dir_all(unit_dir);
    }

    #[tokio::test]
    async fn install_authored_refuses_to_clobber_existing_unit() {
        let unit_dir = test_unit_dir("clobber");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::write(unit_dir.join("placeholder-sync.timer"), "existing").unwrap();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let recorded_invocations = Arc::clone(&invocations);
        let scheduler = SystemdUserScheduler::with_command_runner_and_unit_dir(
            move |invocation, _timeout| {
                recorded_invocations
                    .lock()
                    .unwrap()
                    .push(invocation.clone());
                async move {
                    Ok(CommandOutput {
                        stdout: String::new(),
                    })
                }
            },
            unit_dir.clone(),
        );

        let error = scheduler
            .install_authored(&authored_spec())
            .await
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<SystemJobError>(),
            Some(SystemJobError::UnitAlreadyExists { unit }) if unit == "placeholder-sync.timer"
        ));
        assert!(invocations.lock().unwrap().is_empty());

        let _ = fs::remove_dir_all(unit_dir);
    }

    #[tokio::test]
    async fn delete_authored_disables_removes_units_and_reloads() {
        let unit_dir = test_unit_dir("delete");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::write(unit_dir.join("placeholder-sync.timer"), "timer").unwrap();
        fs::write(unit_dir.join("placeholder-sync.service"), "service").unwrap();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let recorded_invocations = Arc::clone(&invocations);
        let scheduler = SystemdUserScheduler::with_command_runner_and_unit_dir(
            move |invocation, _timeout| {
                recorded_invocations
                    .lock()
                    .unwrap()
                    .push(invocation.clone());
                async move {
                    Ok(CommandOutput {
                        stdout: String::new(),
                    })
                }
            },
            unit_dir.clone(),
        );

        scheduler
            .delete_authored("placeholder-sync.timer")
            .await
            .unwrap();

        assert!(!unit_dir.join("placeholder-sync.timer").exists());
        assert!(!unit_dir.join("placeholder-sync.service").exists());
        let invocations = invocations.lock().unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(
            invocations[0].args,
            vec!["--user", "disable", "--now", "placeholder-sync.timer"]
        );
        assert_eq!(invocations[1].args, vec!["--user", "daemon-reload"]);

        let _ = fs::remove_dir_all(unit_dir);
    }

    #[test]
    fn parses_timer_and_successful_service_show_output() {
        let summary = parse_local_job_summary(TIMER_SHOW, SERVICE_SUCCESS_SHOW).unwrap();

        assert_eq!(summary.unit, "placeholder-cleanup.timer");
        assert_eq!(summary.title, "Placeholder Cleanup");
        assert_eq!(summary.backend, "systemd-user");
        assert!(summary.enabled);
        assert_eq!(summary.unit_file_state, "enabled");
        assert!(summary.manageable);
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
    fn parses_failed_timer_without_triggered_unit_as_degraded_summary() {
        let summary = parse_local_job_summary(FAILED_TIMER_WITHOUT_TRIGGER_SHOW, "").unwrap();

        assert_eq!(summary.unit, "placeholder-failed.timer");
        assert_eq!(summary.active_state, "failed");
        assert_eq!(summary.triggered_unit, "");
        assert!(summary.enabled);
        assert_eq!(summary.unit_file_state, "enabled");
        assert_eq!(summary.schedule.next_elapse_at, Some(1_781_371_800));
        assert_eq!(summary.schedule.raw, "Sat 2026-06-13 17:30:00 UTC");
        assert_eq!(summary.last_fired_at, Some(1_781_370_001));
        assert_eq!(summary.last_exit.code, None);
        assert_eq!(summary.last_exit.result, "unknown");
        assert_eq!(summary.last_exit.at, None);
    }

    #[test]
    fn parses_timer_triggering_non_service_unit_as_degraded_summary() {
        let summary = parse_local_job_summary(TARGET_TIMER_SHOW, "").unwrap();

        assert_eq!(summary.unit, "placeholder-target.timer");
        assert_eq!(summary.triggered_unit, "placeholder.target");
        assert_eq!(summary.last_exit.code, None);
        assert_eq!(summary.last_exit.result, "unknown");
    }

    #[test]
    fn preserves_non_enableable_unit_file_state() {
        let timer_show = TIMER_SHOW.replace("UnitFileState=enabled", "UnitFileState=static");
        let summary = parse_local_job_summary(&timer_show, SERVICE_SUCCESS_SHOW).unwrap();

        assert!(!summary.enabled);
        assert_eq!(summary.unit_file_state, "static");
        assert!(!summary.manageable);
    }

    #[test]
    fn uses_list_timer_next_elapse_for_monotonic_timers() {
        let timer_show = "\
NextElapseUSecRealtime=n/a\n\
NextElapseUSecMonotonic=123456789\n\
LastTriggerUSec=n/a\n\
Id=placeholder-cleanup.timer\n\
Triggers=placeholder-cleanup.service\n\
ActiveState=active\n\
UnitFileState=enabled\n";
        let summary = parse_local_job_summary_with_listed_next(
            timer_show,
            SERVICE_SUCCESS_SHOW,
            Some("Sat 2026-06-13 17:30:00 UTC"),
        )
        .unwrap();

        assert_eq!(summary.schedule.next_elapse_at, Some(1_781_371_800));
        assert_eq!(summary.schedule.raw, "Sat 2026-06-13 17:30:00 UTC");
    }

    #[test]
    fn falls_back_to_service_exit_for_missing_last_trigger() {
        let timer_show = TIMER_SHOW.replace(
            "LastTriggerUSec=Sat 2026-06-13 17:00:01 UTC",
            "LastTriggerUSec=n/a",
        );
        let summary = parse_local_job_summary(&timer_show, SERVICE_SUCCESS_SHOW).unwrap();

        assert_eq!(summary.last_fired_at, Some(1_781_370_002));
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
    fn parses_list_timer_next_elapses() {
        let next_elapses = parse_list_timer_next_elapses(
            "Sat 2026-06-13 17:30:00 UTC 29min Sat 2026-06-13 17:00:01 UTC 1s ago placeholder-cleanup.timer placeholder-cleanup.service\n\
             n/a n/a n/a n/a placeholder-sync.timer placeholder-sync.service\n",
        );

        assert_eq!(
            next_elapses.get("placeholder-cleanup.timer"),
            Some(&"Sat 2026-06-13 17:30:00 UTC".to_string())
        );
        assert_eq!(
            next_elapses.get("placeholder-sync.timer"),
            Some(&String::new())
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
    fn skips_uninstantiated_timer_templates() {
        let units = enumerate_timer_units(
            "",
            "placeholder@.timer disabled disabled\nplaceholder@daily.timer enabled enabled\n",
            &["placeholder@*.timer".to_string()],
        );

        assert_eq!(units, vec!["placeholder@daily.timer".to_string()]);
    }

    #[test]
    fn enumerates_unfiltered_timers_without_allowlist_gate() {
        let units = enumerate_timer_units_unfiltered(
            "placeholder-loaded.timer placeholder-loaded.service\n",
            "placeholder-installed.timer disabled disabled\nplaceholder@.timer disabled disabled\n",
        );

        assert_eq!(
            units,
            vec![
                "placeholder-installed.timer".to_string(),
                "placeholder-loaded.timer".to_string()
            ]
        );
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
    fn rejects_dash_prefixed_unit_names() {
        let result = build_control_invocation(
            LocalJobControl::Run,
            "-placeholder.timer",
            "placeholder.service",
            &["*.timer".to_string()],
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported characters")
        );
    }

    #[test]
    fn rejects_run_now_without_triggered_service_before_building_invocation() {
        let result = build_control_invocation(
            LocalJobControl::Run,
            "placeholder-failed.timer",
            "",
            &["placeholder-*.timer".to_string()],
        );

        let error = result.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SystemJobError>(),
            Some(SystemJobError::MissingTriggeredUnit { unit }) if unit == "placeholder-failed.timer"
        ));
        assert!(
            error
                .to_string()
                .contains("has no triggered service to run")
        );
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
    fn listing_invocations_request_full_unit_names() {
        assert!(
            list_timers_invocation()
                .args
                .contains(&"--full".to_string())
        );
        assert!(
            list_unit_files_invocation()
                .args
                .contains(&"--full".to_string())
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

    #[tokio::test]
    async fn list_jobs_includes_failed_timer_without_trigger_and_healthy_timer() {
        let scheduler =
            SystemdUserScheduler::with_command_runner(|invocation, _timeout| async move {
                if invocation.args.iter().any(|arg| arg == "list-timers") {
                    return Ok(CommandOutput {
                        stdout: LIST_TIMERS_WITH_FAILED_TIMER.to_string(),
                    });
                }
                if invocation.args.iter().any(|arg| arg == "list-unit-files") {
                    return Ok(CommandOutput {
                        stdout: LIST_UNIT_FILES_WITH_FAILED_TIMER.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.timer")
                {
                    return Ok(CommandOutput {
                        stdout: TIMER_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.service")
                {
                    return Ok(CommandOutput {
                        stdout: SERVICE_SUCCESS_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-failed.timer")
                {
                    return Ok(CommandOutput {
                        stdout: FAILED_TIMER_WITHOUT_TRIGGER_SHOW.to_string(),
                    });
                }
                panic!("unexpected invocation: {invocation:?}");
            });

        let summaries = scheduler
            .list_jobs(&["placeholder-*.timer".to_string()])
            .await
            .unwrap();

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].unit, "placeholder-cleanup.timer");
        assert_eq!(summaries[0].triggered_unit, "placeholder-cleanup.service");
        assert_eq!(summaries[1].unit, "placeholder-failed.timer");
        assert_eq!(summaries[1].active_state, "failed");
        assert_eq!(summaries[1].triggered_unit, "");
        assert!(summaries.iter().all(|summary| summary.managed));
    }

    #[tokio::test]
    async fn available_jobs_returns_all_timers_with_managed_flags() {
        let scheduler =
            SystemdUserScheduler::with_command_runner(|invocation, _timeout| async move {
                if invocation.args.iter().any(|arg| arg == "list-timers") {
                    return Ok(CommandOutput {
                        stdout: format!(
                            "{}n/a n/a n/a n/a placeholder-target.timer placeholder.target\n",
                            LIST_TIMERS_WITH_AVAILABLE_TIMERS
                        ),
                    });
                }
                if invocation.args.iter().any(|arg| arg == "list-unit-files") {
                    return Ok(CommandOutput {
                        stdout: format!(
                            "{}placeholder-target.timer disabled disabled\n",
                            LIST_UNIT_FILES_WITH_AVAILABLE_TIMERS
                        ),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.timer")
                {
                    return Ok(CommandOutput {
                        stdout: TIMER_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.service")
                {
                    return Ok(CommandOutput {
                        stdout: SERVICE_SUCCESS_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-discovered.timer")
                {
                    return Ok(CommandOutput {
                        stdout: DISCOVERED_TIMER_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-discovered.service")
                {
                    return Ok(CommandOutput {
                        stdout: SERVICE_SUCCESS_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-failed.timer")
                {
                    return Ok(CommandOutput {
                        stdout: FAILED_TIMER_WITHOUT_TRIGGER_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-target.timer")
                {
                    return Ok(CommandOutput {
                        stdout: TARGET_TIMER_SHOW.to_string(),
                    });
                }
                panic!("unexpected invocation: {invocation:?}");
            });

        let summaries = scheduler
            .available_jobs(&["placeholder-cleanup.timer".to_string()])
            .await
            .unwrap();

        assert_eq!(summaries.len(), 4);
        assert_eq!(summaries[0].unit, "placeholder-cleanup.timer");
        assert!(summaries[0].managed);
        assert_eq!(summaries[1].unit, "placeholder-discovered.timer");
        assert!(!summaries[1].managed);
        assert_eq!(summaries[2].unit, "placeholder-failed.timer");
        assert_eq!(summaries[2].active_state, "failed");
        assert!(!summaries[2].managed);
        assert_eq!(summaries[3].unit, "placeholder-target.timer");
        assert_eq!(summaries[3].triggered_unit, "placeholder.target");
        assert!(!summaries[3].managed);
    }

    #[tokio::test]
    async fn discovered_but_unmanaged_control_is_rejected_before_systemctl_mutation() {
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let recorded_invocations = Arc::clone(&invocations);
        let scheduler = SystemdUserScheduler::with_command_runner(move |invocation, _timeout| {
            recorded_invocations
                .lock()
                .unwrap()
                .push(invocation.clone());
            async move {
                panic!("unexpected invocation: {invocation:?}");
            }
        });

        let error = scheduler
            .control_job(
                "placeholder-discovered.timer",
                LocalJobControl::Disable,
                &[],
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<SystemJobError>(),
            Some(SystemJobError::NotAllowlisted { unit }) if unit == "placeholder-discovered.timer"
        ));
        assert!(invocations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_jobs_skips_generic_single_unit_errors_without_failing_list() {
        let scheduler =
            SystemdUserScheduler::with_command_runner(|invocation, _timeout| async move {
                if invocation.args.iter().any(|arg| arg == "list-timers") {
                    return Ok(CommandOutput {
                        stdout: LIST_TIMERS_WITH_BROKEN_TIMER.to_string(),
                    });
                }
                if invocation.args.iter().any(|arg| arg == "list-unit-files") {
                    return Ok(CommandOutput {
                        stdout: LIST_UNIT_FILES_WITH_BROKEN_TIMER.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.timer")
                {
                    return Ok(CommandOutput {
                        stdout: TIMER_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-cleanup.service")
                {
                    return Ok(CommandOutput {
                        stdout: SERVICE_SUCCESS_SHOW.to_string(),
                    });
                }
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-broken.timer")
                {
                    return Err(anyhow::anyhow!("unit show failed"));
                }
                panic!("unexpected invocation: {invocation:?}");
            });

        let summaries = scheduler
            .list_jobs(&["placeholder-*.timer".to_string()])
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].unit, "placeholder-cleanup.timer");
    }

    #[tokio::test]
    async fn run_now_without_triggered_service_returns_typed_error_without_starting_service() {
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let recorded_invocations = Arc::clone(&invocations);
        let scheduler = SystemdUserScheduler::with_command_runner(move |invocation, _timeout| {
            recorded_invocations
                .lock()
                .unwrap()
                .push(invocation.clone());
            async move {
                if invocation
                    .args
                    .iter()
                    .any(|arg| arg == "placeholder-failed.timer")
                {
                    return Ok(CommandOutput {
                        stdout: FAILED_TIMER_WITHOUT_TRIGGER_SHOW.to_string(),
                    });
                }
                panic!("unexpected invocation: {invocation:?}");
            }
        });

        let error = scheduler
            .control_job(
                "placeholder-failed.timer",
                LocalJobControl::Run,
                &["placeholder-*.timer".to_string()],
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<SystemJobError>(),
            Some(SystemJobError::MissingTriggeredUnit { unit }) if unit == "placeholder-failed.timer"
        ));
        let invocations = invocations.lock().unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].args.iter().any(|arg| arg == "show"));
        assert!(
            !invocations
                .iter()
                .any(|invocation| invocation.args.iter().any(|arg| arg == "start"))
        );
    }
}
