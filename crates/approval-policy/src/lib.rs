use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalVerdict {
    pub approve: bool,
    pub reason: String,
    pub target_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedApprovalDecision {
    Allow(ScopedApprovalOutcome),
    Deny(ScopedApprovalOutcome),
    Escalate(ScopedApprovalOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedApprovalOutcome {
    pub reason: String,
    pub target_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownToolDisposition {
    Deny,
    Escalate,
}

impl ScopedApprovalDecision {
    pub fn outcome(&self) -> &ScopedApprovalOutcome {
        match self {
            ScopedApprovalDecision::Allow(outcome)
            | ScopedApprovalDecision::Deny(outcome)
            | ScopedApprovalDecision::Escalate(outcome) => outcome,
        }
    }
}

pub fn evaluate_harness_approval(
    tool_id: &str,
    args_json: &Value,
    worktree: &Path,
) -> ApprovalVerdict {
    match evaluate_scoped_approval(tool_id, args_json, worktree, UnknownToolDisposition::Deny) {
        ScopedApprovalDecision::Allow(outcome) => approve(&outcome.reason, outcome.target_paths),
        ScopedApprovalDecision::Deny(outcome) | ScopedApprovalDecision::Escalate(outcome) => {
            deny(&outcome.reason, outcome.target_paths)
        }
    }
}

pub fn evaluate_autonomous_approval(
    tool_id: &str,
    args_json: &Value,
    worktree: &Path,
) -> ScopedApprovalDecision {
    evaluate_scoped_approval(
        tool_id,
        args_json,
        worktree,
        UnknownToolDisposition::Escalate,
    )
}

pub fn evaluate_scoped_approval(
    tool_id: &str,
    args_json: &Value,
    worktree: &Path,
    unknown_tool_disposition: UnknownToolDisposition,
) -> ScopedApprovalDecision {
    match tool_id {
        "project.inspect" => allow("safe project inspection", Vec::new()),
        "fs.list" | "fs.read_text" | "rg.search" | "git.status" | "git.diff" => {
            approve_if_paths_scoped(
                args_json,
                worktree,
                read_path_fields(tool_id),
                "safe read/inspect tool",
            )
        }
        "fs.apply_patch" | "fs.write_text" | "fs.mkdir" => approve_if_paths_scoped(
            args_json,
            worktree,
            &["path"],
            "worktree-local file mutation",
        ),
        "fs.move" => approve_if_paths_scoped(
            args_json,
            worktree,
            &["from_path", "to_path"],
            "worktree-local file move",
        ),
        "command.run" => evaluate_command_run(args_json, worktree),
        "python.run" => evaluate_python_run(args_json, worktree),
        "tests.run" => evaluate_tests_run(args_json, worktree),
        other if other.starts_with("github.") => {
            deny_decision("GitHub tools are denied by scoped autonomy", Vec::new())
        }
        "git.stage_patch" => deny_decision(
            "git.stage_patch is denied to prevent publication flow",
            Vec::new(),
        ),
        other if tool_name_is_publication(other) => {
            deny_decision("publication/release tool is denied", Vec::new())
        }
        other => match unknown_tool_disposition {
            UnknownToolDisposition::Deny => deny_decision(
                &format!("tool '{other}' is not in the harness allow-list"),
                Vec::new(),
            ),
            UnknownToolDisposition::Escalate => escalate(
                &format!("tool '{other}' is not in the autonomous safe set"),
                Vec::new(),
            ),
        },
    }
}

pub fn read_path_fields(tool_id: &str) -> &'static [&'static str] {
    match tool_id {
        "fs.read_text" => &["path"],
        "fs.list" | "rg.search" | "git.diff" => &["path", "pathspec"],
        _ => &[],
    }
}

pub fn command_text(args: &Value) -> String {
    normalize_command_run_like_args(args)
        .map(|command| command.display())
        .unwrap_or_default()
}

pub fn tool_call_command_text(tool_id: &str, args_json: &Value) -> String {
    let command = if tool_id == "tests.run" {
        normalize_tests_run_like_args(args_json)
    } else {
        normalize_command_run_like_args(args_json)
    };
    command
        .map(|command| command_policy_command(&command).unwrap_or_else(|_| command.display()))
        .unwrap_or_default()
}

pub fn command_looks_like_read_build_or_test(command: &str) -> bool {
    let allowed = [
        "printf",
        "/usr/bin/printf",
        "echo",
        "true",
        "pwd",
        "cat",
        "ls",
        "head",
        "tail",
        "wc",
        "find",
        "rg",
        "grep",
        "git status",
        "git diff",
        "git log",
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm run test",
        "npm test",
        "npm build",
        "pnpm test",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "yarn run test",
        "bun test",
        "pytest",
        "go test",
        "make test",
        "just test",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed)
}

pub fn command_looks_like_validation(command: &str) -> bool {
    let allowed = [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm run test",
        "npm test",
        "npm build",
        "pnpm test",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "yarn run test",
        "bun test",
        "pytest",
        "go test",
        "make test",
        "just test",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed) || command_is_focused_node_test(command)
}

pub fn command_session_looks_like_check_validation(command: &str) -> bool {
    let allowed = [
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm build",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed)
}

pub fn command_has_shell_escape_or_write(command: &str) -> bool {
    let lower = command.to_lowercase();
    let denied_needles = [
        ";",
        "&&",
        "||",
        "|",
        ">",
        "<",
        "\n",
        "`",
        // Autonomous shell commands do not trust environment expansion as a stable scope boundary.
        "$",
        "$(",
        "../",
        " rm ",
        " rm -",
        " mv ",
        " cp ",
        " tee ",
        " -delete",
        " -exec",
        " -execdir",
        "sed -i",
        "perl -pi",
        "python -c",
        "python3 -c",
    ];
    denied_needles.iter().any(|needle| lower.contains(needle))
}

pub fn tool_name_is_publication(tool: &str) -> bool {
    let lower = tool.to_lowercase();
    lower.contains("publish") || lower.contains("publication") || lower.contains("release")
}

pub fn normalize_command_run_like_args(args: &Value) -> Option<NormalizedCommandRun> {
    normalize_command_like_args(args, false)
}

pub fn normalize_tests_run_like_args(args: &Value) -> Option<NormalizedCommandRun> {
    normalize_command_like_args(args, true)
}

pub fn command_policy_command(command: &NormalizedCommandRun) -> Result<String, String> {
    command_policy_command_inner(command, 0)
}

fn approve(reason: &str, target_paths: Vec<String>) -> ApprovalVerdict {
    ApprovalVerdict {
        approve: true,
        reason: reason.to_string(),
        target_paths,
    }
}

fn deny(reason: &str, target_paths: Vec<String>) -> ApprovalVerdict {
    ApprovalVerdict {
        approve: false,
        reason: reason.to_string(),
        target_paths,
    }
}

fn allow(reason: &str, target_paths: Vec<String>) -> ScopedApprovalDecision {
    ScopedApprovalDecision::Allow(ScopedApprovalOutcome {
        reason: reason.to_string(),
        target_paths,
    })
}

fn deny_decision(reason: &str, target_paths: Vec<String>) -> ScopedApprovalDecision {
    ScopedApprovalDecision::Deny(ScopedApprovalOutcome {
        reason: reason.to_string(),
        target_paths,
    })
}

fn escalate(reason: &str, target_paths: Vec<String>) -> ScopedApprovalDecision {
    ScopedApprovalDecision::Escalate(ScopedApprovalOutcome {
        reason: reason.to_string(),
        target_paths,
    })
}

fn approve_if_paths_scoped(
    args: &Value,
    worktree: &Path,
    fields: &[&str],
    reason: &str,
) -> ScopedApprovalDecision {
    let paths = extract_paths(args, fields);
    for path in &paths {
        if !path_is_inside_worktree(worktree, path) {
            return deny_decision(
                &format!(
                    "path '{}' resolves outside the worker worktree",
                    path.display()
                ),
                stringify_paths(paths),
            );
        }
    }
    allow(reason, stringify_paths(paths))
}

fn evaluate_command_run(args: &Value, worktree: &Path) -> ScopedApprovalDecision {
    let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny_decision(
            "command cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(args) {
        return deny_decision("command requests network access", vec![cwd.to_string()]);
    }
    if has_conflicting_command_aliases(args) {
        return deny_decision(
            "command.run specifies both command and cmd aliases",
            vec![cwd.to_string()],
        );
    }
    let Some(command) = normalize_command_run_like_args(args) else {
        return deny_decision("command.run command is missing", vec![cwd.to_string()]);
    };
    let policy_command = match command_policy_command(&command) {
        Ok(command) => command,
        Err(reason) => {
            return deny_decision(
                &format!("command shell wrapper is not a single simple command: {reason}"),
                vec![cwd.to_string()],
            );
        }
    };
    if command_is_publication_or_network_mutating(&policy_command) {
        return deny_decision(
            "command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_uses_external_git_diff(&policy_command) || env_requests_external_git_diff(args) {
        return deny_decision(
            "command requests an external git diff helper",
            vec![cwd.to_string()],
        );
    }
    if command_uses_git_output_file(&policy_command)
        || command_uses_cargo_clippy_fix(&policy_command)
    {
        return deny_decision(
            "command requests write-like output or auto-fix behavior",
            vec![cwd.to_string()],
        );
    }
    if git_diff_or_log_missing_helper_disables(&policy_command) {
        return deny_decision(
            "command git diff/log must disable external diff helpers",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&policy_command) {
        return deny_decision(
            "command contains shell control, path traversal, or write-like operations",
            vec![cwd.to_string()],
        );
    }
    if command_has_external_path_reference(args, worktree, Path::new(cwd)) {
        return deny_decision(
            "command references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(args, worktree) {
        return deny_decision(
            "command env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if !command_looks_like_read_build_or_test(&policy_command) {
        return escalate(
            "command program is not on the safe read/build/test allow-list",
            vec![cwd.to_string()],
        );
    }
    allow(
        "bounded read/build/test command in worker worktree",
        vec![cwd.to_string()],
    )
}

fn evaluate_python_run(args: &Value, worktree: &Path) -> ScopedApprovalDecision {
    let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny_decision(
            "python cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(args) {
        return deny_decision("python.run requests network access", vec![cwd.to_string()]);
    }
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    if script.trim().is_empty() {
        return deny_decision("python.run script is missing", vec![cwd.to_string()]);
    }
    if args_have_external_path_reference(args, worktree) {
        return deny_decision(
            "python.run args reference a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(args, worktree) {
        return deny_decision(
            "python.run env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if script_contains_external_or_network_mutation(&script) {
        return deny_decision(
            "python script is not clearly scoped to the worker worktree",
            vec![cwd.to_string()],
        );
    }
    allow(
        "bounded python.run in worker worktree",
        vec![cwd.to_string()],
    )
}

fn evaluate_tests_run(args: &Value, worktree: &Path) -> ScopedApprovalDecision {
    let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny_decision(
            "tests.run cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(args) {
        return deny_decision("tests.run requests network access", vec![cwd.to_string()]);
    }
    if has_conflicting_command_aliases(args) {
        return deny_decision(
            "tests.run specifies both command and cmd aliases",
            vec![cwd.to_string()],
        );
    }
    let Some(command) = normalize_tests_run_like_args(args) else {
        return deny_decision("tests.run command is missing", vec![cwd.to_string()]);
    };
    let policy_command = match command_policy_command(&command) {
        Ok(command) => command,
        Err(reason) => {
            return deny_decision(
                &format!("tests.run shell wrapper is not a single simple command: {reason}"),
                vec![cwd.to_string()],
            );
        }
    };
    if command_is_publication_or_network_mutating(&policy_command) {
        return deny_decision(
            "tests.run command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_uses_external_git_diff(&policy_command) || env_requests_external_git_diff(args) {
        return deny_decision(
            "tests.run command requests an external git diff helper",
            vec![cwd.to_string()],
        );
    }
    if command_uses_git_output_file(&policy_command)
        || command_uses_cargo_clippy_fix(&policy_command)
    {
        return deny_decision(
            "tests.run command requests write-like output or auto-fix behavior",
            vec![cwd.to_string()],
        );
    }
    if git_diff_or_log_missing_helper_disables(&policy_command) {
        return deny_decision(
            "tests.run git diff/log must disable external diff helpers",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&policy_command) {
        return deny_decision(
            "tests.run command contains shell control, path traversal, or write-like operations",
            vec![cwd.to_string()],
        );
    }
    if normalized_command_has_external_path_reference(&command, args, worktree, Path::new(cwd)) {
        return deny_decision(
            "tests.run command references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(args, worktree) {
        return deny_decision(
            "tests.run env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if !command_looks_like_validation(&policy_command) {
        return escalate(
            "tests.run command program is not on the safe test allow-list",
            vec![cwd.to_string()],
        );
    }
    allow(
        "bounded tests.run in worker worktree",
        vec![cwd.to_string()],
    )
}

fn extract_paths(args: &Value, fields: &[&str]) -> Vec<PathBuf> {
    fields
        .iter()
        .filter_map(|field| args.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn stringify_paths(paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn path_is_inside_worktree(worktree: &Path, candidate: &Path) -> bool {
    let base = normalize_path(worktree);
    let joined = if candidate.is_absolute() {
        normalize_path(candidate)
    } else {
        normalize_path(&worktree.join(candidate))
    };
    if let Ok(canonical_base) = fs::canonicalize(&base) {
        return canonical_path_is_inside(&canonical_base, &joined);
    }
    joined == base || joined.starts_with(base)
}

fn canonical_path_is_inside(canonical_base: &Path, candidate: &Path) -> bool {
    if let Ok(canonical_candidate) = fs::canonicalize(candidate) {
        return canonical_candidate == canonical_base
            || canonical_candidate.starts_with(canonical_base);
    }

    let Some((existing_ancestor, canonical_ancestor)) = nearest_existing_ancestor(candidate) else {
        return false;
    };
    if canonical_ancestor != canonical_base && !canonical_ancestor.starts_with(canonical_base) {
        return false;
    }

    let suffix = candidate
        .strip_prefix(&existing_ancestor)
        .unwrap_or_else(|_| Path::new(""));
    let resolved = normalize_path(&canonical_ancestor.join(suffix));
    resolved == canonical_base || resolved.starts_with(canonical_base)
}

fn nearest_existing_ancestor(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(current) {
            return Some((current.to_path_buf(), canonical));
        }
        current = current.parent()?;
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCommandRun {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_secs: Option<u64>,
}

impl NormalizedCommandRun {
    pub fn display(&self) -> String {
        let mut parts = vec![self.command.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

fn normalize_command_like_args(
    args: &Value,
    split_command_string: bool,
) -> Option<NormalizedCommandRun> {
    let object = args.as_object()?;
    let (command_key, command_value) = if let Some(value) = object.get("cmd") {
        ("cmd", value)
    } else {
        ("command", object.get("command")?)
    };
    let explicit_args = object.get("args").filter(|value| !value.is_null());
    let mut normalized_args = explicit_args
        .and_then(command_args_value)
        .unwrap_or_default();
    let (command, mut argv) = match command_value {
        Value::String(command) => {
            let command = command.trim();
            if command.is_empty() {
                return None;
            }
            if split_command_string {
                let mut parts = command_args_value(&Value::String(command.to_string()))?;
                let command = parts.first().cloned()?;
                let argv = parts.drain(1..).collect();
                (command, argv)
            } else if command_key == "cmd"
                || (explicit_args.is_none() && command_string_looks_like_shell(command))
            {
                (
                    "sh".to_string(),
                    vec!["-lc".to_string(), command.to_string()],
                )
            } else {
                (command.to_string(), Vec::new())
            }
        }
        Value::Array(values) => {
            let mut parts = command_args_value(&Value::Array(values.clone()))?;
            let command = parts.first().cloned()?;
            let argv = if explicit_args.is_some() && !split_command_string {
                Vec::new()
            } else {
                parts.drain(1..).collect()
            };
            (command, argv)
        }
        _ => return None,
    };

    if !normalized_args.is_empty() {
        argv.append(&mut normalized_args);
    }

    Some(NormalizedCommandRun {
        command,
        args: argv,
        timeout_secs: command_timeout_secs(args),
    })
}

fn command_args_value(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.into_iter().map(str::to_string).collect()),
        Value::String(value) => Some(
            value
                .split_whitespace()
                .filter(|part| !part.trim().is_empty())
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

fn command_timeout_secs(args: &Value) -> Option<u64> {
    args.get("timeout_secs")
        .and_then(json_value_to_u64_lossy)
        .or_else(|| {
            args.get("timeout_seconds")
                .and_then(json_value_to_u64_lossy)
        })
        .or_else(|| {
            let millis = args.get("timeout_ms").and_then(json_value_to_u64_lossy)?;
            Some(millis.saturating_add(999) / 1_000).map(|seconds| seconds.max(1))
        })
}

fn json_value_to_u64_lossy(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn command_string_looks_like_shell(command: &str) -> bool {
    command.chars().any(char::is_whitespace)
        || command.chars().any(|character| {
            matches!(
                character,
                ';' | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '$'
                    | '`'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '~'
            )
        })
}

fn has_conflicting_command_aliases(args: &Value) -> bool {
    command_alias_present(args.get("command")) && command_alias_present(args.get("cmd"))
}

fn command_alias_present(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .any(|value| !value.trim().is_empty()),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

pub fn network_policy_allows_network(args: &Value) -> bool {
    args.get("network_policy")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| !matches!(value, "inherit" | "none" | "disabled" | "off" | "deny"))
}

fn command_is_publication_or_network_mutating(command: &str) -> bool {
    let lower = command.to_lowercase();
    let denied_needles = [
        "git push",
        "git commit",
        "git tag",
        "git add",
        "git stage",
        "git switch -c",
        "git checkout -b",
        "gh pr",
        "gh release",
        "npm publish",
        "pnpm publish",
        "yarn publish",
        "cargo publish",
        "docker push",
        "curl ",
        "wget ",
        "scp ",
        "rsync ",
        "ssh ",
        "npm install",
        "pnpm install",
        "yarn install",
        "bun install",
    ];
    denied_needles.iter().any(|needle| lower.contains(needle))
}

fn command_uses_external_git_diff(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let runs_external_diff_command = tokens.iter().any(|token| matches!(*token, "diff" | "log"));
    runs_external_diff_command
        && (tokens.iter().any(|token| {
            *token == "--ext-diff"
                || token.starts_with("--ext-diff=")
                || token.starts_with("--config=diff.external")
                || token.starts_with("diff.external")
        }) || tokens
            .windows(2)
            .any(|window| window[0] == "-c" && window[1].starts_with("diff.external")))
}

fn command_uses_git_output_file(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let writes_output = tokens
        .iter()
        .any(|token| *token == "--output" || token.starts_with("--output="));
    writes_output && tokens.iter().any(|token| matches!(*token, "diff" | "log"))
}

fn git_diff_or_log_missing_helper_disables(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let runs_diff_or_log = tokens.iter().any(|token| matches!(*token, "diff" | "log"));
    if !runs_diff_or_log {
        return false;
    }
    if tokens.iter().any(|token| *token == "--textconv") {
        return true;
    }
    !(tokens.iter().any(|token| *token == "--no-ext-diff")
        && tokens.iter().any(|token| *token == "--no-textconv"))
}

fn command_uses_cargo_clippy_fix(command: &str) -> bool {
    let lower = command.to_lowercase();
    let tokens = lower.split_whitespace().collect::<Vec<_>>();
    tokens.first() == Some(&"cargo")
        && tokens.iter().any(|token| *token == "clippy")
        && tokens.iter().any(|token| *token == "--fix")
}

fn env_requests_external_git_diff(args: &Value) -> bool {
    let Some(env) = args.get("env").and_then(Value::as_object) else {
        return false;
    };
    env.iter().any(|(key, value)| {
        let key = key.to_ascii_uppercase();
        if key == "GIT_EXTERNAL_DIFF" {
            return true;
        }
        let Some(value) = env_value_text(value) else {
            return false;
        };
        let value = value.to_ascii_lowercase();
        (key == "GIT_CONFIG_PARAMETERS" || key.starts_with("GIT_CONFIG_KEY_"))
            && value.contains("diff.external")
    })
}

fn env_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn command_policy_command_inner(
    command: &NormalizedCommandRun,
    depth: usize,
) -> Result<String, String> {
    if depth > 2 {
        return Err("too many nested shell wrappers".to_string());
    }
    if !is_shell_program(&command.command) {
        return Ok(command.display());
    }
    let Some(flag) = command.args.first().map(String::as_str) else {
        return Err("shell wrapper missing -c/-lc".to_string());
    };
    if !matches!(flag, "-c" | "-lc") {
        return Err("shell wrapper must use -c or -lc".to_string());
    }
    if command.args.len() != 2 {
        return Err("shell wrapper must contain exactly one inner command".to_string());
    }
    let inner = command.args[1].trim();
    if inner.is_empty() {
        return Err("shell wrapper inner command is empty".to_string());
    }
    if shell_string_has_unsafe_control(inner) {
        return Err("inner command contains shell control".to_string());
    }
    let tokens = shell_words(inner)?;
    if tokens.is_empty() {
        return Err("inner command is empty".to_string());
    }
    let nested = NormalizedCommandRun {
        command: tokens[0].clone(),
        args: tokens.into_iter().skip(1).collect(),
        timeout_secs: command.timeout_secs,
    };
    command_policy_command_inner(&nested, depth + 1)
}

fn is_shell_program(program: &str) -> bool {
    matches!(
        program,
        "sh" | "bash" | "/bin/sh" | "/usr/bin/sh" | "/bin/bash" | "/usr/bin/bash"
    )
}

fn shell_string_has_unsafe_control(command: &str) -> bool {
    if command.contains('\\') {
        return true;
    }
    let chars = command.chars();
    let mut quote: Option<char> = None;
    for ch in chars {
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                ';' | '|' | '>' | '<' | '`' | '$' | '{' | '}' | '\n' => return true,
                '&' => return true,
                _ => {}
            },
        }
    }
    quote.is_some()
}

pub fn shell_words(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match quote {
            Some(current_quote) if ch == current_quote => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".to_string());
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

fn command_has_external_path_reference(args: &Value, worktree: &Path, cwd: &Path) -> bool {
    let Some(command) = normalize_command_run_like_args(args) else {
        return command_tokens_have_external_path_reference(command_tokens(args), worktree, cwd);
    };
    normalized_command_has_external_path_reference(&command, args, worktree, cwd)
}

fn normalized_command_has_external_path_reference(
    command: &NormalizedCommandRun,
    args: &Value,
    worktree: &Path,
    cwd: &Path,
) -> bool {
    let tokens = command_policy_command(command)
        .ok()
        .and_then(|command| shell_words(&command).ok())
        .unwrap_or_else(|| command_tokens(args));
    command_tokens_have_external_path_reference(tokens, worktree, cwd)
}

fn command_tokens_have_external_path_reference(
    tokens: Vec<String>,
    worktree: &Path,
    cwd: &Path,
) -> bool {
    tokens
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 0)
        .filter_map(|(_, token)| path_like_command_token(token))
        .map(|path| command_path_argument_for_cwd(cwd, path))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn command_path_argument_for_cwd(cwd: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn args_have_external_path_reference(args: &Value, worktree: &Path) -> bool {
    args.get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|value| value.split_whitespace().map(clean_command_token))
        .filter_map(|token| path_like_command_token(&token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn env_has_external_path_reference(args: &Value, worktree: &Path) -> bool {
    args.get("env")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|env| env.values())
        .filter_map(Value::as_str)
        .flat_map(env_path_tokens)
        .filter_map(|token| path_like_command_token(&token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn env_path_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| ch.is_whitespace() || ch == ':')
        .filter(|token| !token.trim().is_empty())
        .map(clean_command_token)
        .collect()
}

fn command_tokens(args: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    for field in ["command", "cmd"] {
        if let Some(alias) = args.get(field) {
            tokens.extend(command_alias_tokens(alias));
        }
    }
    if let Some(values) = args.get("args").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str) {
            tokens.extend(value.split_whitespace().map(clean_command_token));
        }
    }
    tokens
}

fn command_alias_tokens(value: &Value) -> Vec<String> {
    match value {
        Value::String(command) => command
            .split_whitespace()
            .map(clean_command_token)
            .collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .flat_map(|value| value.split_whitespace().map(clean_command_token))
            .collect(),
        _ => Vec::new(),
    }
}

fn clean_command_token(token: &str) -> String {
    token
        .trim_matches(|ch| matches!(ch, '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']'))
        .to_string()
}

fn path_like_command_token(token: &str) -> Option<PathBuf> {
    let raw = token.trim();
    if raw.starts_with('-') {
        return raw
            .split('=')
            .skip(1)
            .find_map(|value| path_like_option_value(value.trim()));
    }
    path_like_value(raw)
}

fn path_like_option_value(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    if token.starts_with('~') {
        return Some(PathBuf::from("/~"));
    }
    if token.starts_with('/') || token.starts_with("../") || token == ".." || token.contains('/') {
        return Some(PathBuf::from(token));
    }
    if token.contains("/../") || token.ends_with("/..") {
        return Some(PathBuf::from(token));
    }
    None
}

fn path_like_value(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    if token.starts_with('~') {
        return Some(PathBuf::from("/~"));
    }
    Some(PathBuf::from(token))
}

fn command_is_focused_node_test(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    command_policy_candidates(&normalized)
        .iter()
        .filter_map(|candidate| shell_words(candidate).ok())
        .any(|tokens| focused_node_test_tokens(&tokens))
}

fn focused_node_test_tokens(tokens: &[String]) -> bool {
    if tokens.len() != 3 || tokens[0] != "node" || tokens[1] != "--test" {
        return false;
    }
    let path = tokens[2].as_str();
    if path.is_empty()
        || path.starts_with('-')
        || path
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return false;
    }
    let file_name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let name = file_name.to_lowercase();
    let has_test_marker = name.contains(".test.") || name.contains(".spec.");
    let has_supported_extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            matches!(
                extension.to_lowercase().as_str(),
                "js" | "mjs" | "cjs" | "ts" | "mts" | "cts"
            )
        })
        .unwrap_or(false);
    has_test_marker && has_supported_extension
}

fn command_matches_allowed_prefix(command: &str, allowed: &[&str]) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let candidates = command_policy_candidates(&normalized);
    allowed.iter().any(|prefix| {
        let prefix = prefix.to_lowercase();
        candidates
            .iter()
            .any(|candidate| candidate == &prefix || candidate.starts_with(&format!("{prefix} ")))
    })
}

fn command_policy_candidates(normalized: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_command_policy_candidate(&mut candidates, normalized);
    for prefix in ["sh -lc ", "sh -c ", "bash -lc ", "bash -c "] {
        if let Some(unwrapped) = normalized.strip_prefix(prefix) {
            push_command_policy_candidate(&mut candidates, unwrapped);
        }
    }
    candidates
}

fn push_command_policy_candidate(candidates: &mut Vec<String>, command: &str) {
    let command = command.to_string();
    if !candidates.contains(&command) {
        candidates.push(command.clone());
    }
    if let Some(stripped) = strip_npm_workspace_flags(&command) {
        if !candidates.contains(&stripped) {
            candidates.push(stripped);
        }
    }
}

fn strip_npm_workspace_flags(command: &str) -> Option<String> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    if parts.first().copied() != Some("npm") {
        return None;
    }

    let mut stripped = vec!["npm"];
    let mut index = 1;
    let mut changed = false;
    while index < parts.len() {
        match parts[index] {
            "--workspace" | "-w" => {
                changed = true;
                index += 2;
            }
            "--workspaces" => {
                changed = true;
                index += 1;
            }
            part if part.starts_with("--workspace=") || part.starts_with("-w=") => {
                changed = true;
                index += 1;
            }
            part => {
                stripped.push(part);
                index += 1;
            }
        }
    }

    changed.then(|| stripped.join(" "))
}

fn script_contains_external_or_network_mutation(script: &str) -> bool {
    let compact: String = script.chars().filter(|ch| !ch.is_whitespace()).collect();
    let denied = [
        "requests.",
        "urllib",
        "http://",
        "https://",
        "subprocess",
        "__import__(",
        "importlib",
        "getattr(",
        "eval(",
        "exec(",
        "socket",
        "os.system",
        "os.popen",
        ".system(",
        ".popen(",
        "os.listdir",
        "shutil.",
        "git push",
        "git commit",
        "gh pr",
        "npm publish",
        "cargo publish",
        "../",
        "..\\",
        "path('..')",
        "path(\"..\")",
        "open('..')",
        "open(\"..\")",
        "'/",
        "\"/",
        "path.home",
        ".home()",
        "expanduser",
        "os.environ",
        "os.getenv",
        "getenv(",
        "environ[",
        ".parent",
        "parents[",
        // Keep python.run conservative: file reads should use fs.read_text so path scope is explicit.
        "open(",
        "open('/",
        "path('/",
        "path(\"/",
        "write_text(",
        "write_bytes(",
        ".write(",
        "touch(",
        "mkdir(",
        "rename(",
        "replace(",
        "remove(",
        "unlink(",
        "rmdir(",
        "rmtree(",
        "unlink('/",
        "rmtree('/",
    ];
    denied
        .iter()
        .any(|needle| script.contains(needle) || compact.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn worktree() -> &'static Path {
        Path::new("/tmp/nucleus-policy-worktree")
    }

    #[test]
    fn allows_worktree_scoped_read() {
        let decision =
            evaluate_autonomous_approval("fs.read_text", &json!({"path":"src/lib.rs"}), worktree());
        assert!(matches!(decision, ScopedApprovalDecision::Allow(_)));
    }

    #[test]
    fn allows_worktree_scoped_patch() {
        let decision = evaluate_autonomous_approval(
            "fs.apply_patch",
            &json!({"path":"src/lib.rs","patch":"@@\n"}),
            worktree(),
        );
        assert!(matches!(decision, ScopedApprovalDecision::Allow(_)));
    }

    #[test]
    fn allows_shell_wrapped_cargo_test() {
        let decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"sh","args":["-lc","cargo test -p nucleus-core"],"cwd":"."}),
            worktree(),
        );
        assert!(matches!(decision, ScopedApprovalDecision::Allow(_)));
    }

    #[test]
    fn denies_publication_and_network_tools() {
        for (tool, args) in [
            ("github.comment", json!({})),
            ("git.stage_patch", json!({"path":"src/lib.rs"})),
            ("release.publish", json!({})),
            (
                "command.run",
                json!({"command":"git","args":["push"],"cwd":"."}),
            ),
            (
                "command.run",
                json!({"command":"curl","args":["https://example.com"],"cwd":"."}),
            ),
            (
                "command.run",
                json!({"command":"cargo","args":["test"],"cwd":".","network_policy":"enabled"}),
            ),
        ] {
            let decision = evaluate_autonomous_approval(tool, &args, worktree());
            assert!(
                matches!(decision, ScopedApprovalDecision::Deny(_)),
                "{tool} should be denied, got {decision:?}"
            );
        }
    }

    #[test]
    fn denies_path_outside_worktree() {
        let decision = evaluate_autonomous_approval(
            "fs.write_text",
            &json!({"path":"/tmp/outside.txt","content":"no"}),
            worktree(),
        );
        assert!(matches!(decision, ScopedApprovalDecision::Deny(_)));
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlink_escape_for_worktree_mutation() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "nucleus-policy-symlink-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "nucleus-policy-symlink-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&root).expect("worktree root should be created");
        fs::create_dir_all(&outside).expect("outside root should be created");
        symlink(&outside, root.join("link")).expect("symlink should be created");

        let decision = evaluate_autonomous_approval(
            "fs.write_text",
            &json!({"path":"link/escape.txt","content":"no"}),
            &root,
        );
        assert!(matches!(decision, ScopedApprovalDecision::Deny(_)));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlink_escape_for_command_path_argument() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "nucleus-policy-command-symlink-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "nucleus-policy-command-symlink-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(root.join("src")).expect("worktree src should be created");
        fs::create_dir_all(&outside).expect("outside root should be created");
        fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n")
            .expect("in-worktree file should be created");
        fs::write(outside.join("secret"), "outside\n").expect("outside file should be created");
        symlink(&outside, root.join("link")).expect("symlink should be created");

        let outside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["link/secret"],"cwd":"."}),
            &root,
        );
        assert!(
            matches!(outside_decision, ScopedApprovalDecision::Deny(_)),
            "symlinked command path argument should be denied, got {outside_decision:?}"
        );

        let inside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["src/lib.rs"],"cwd":"."}),
            &root,
        );
        assert!(
            matches!(inside_decision, ScopedApprovalDecision::Allow(_)),
            "in-worktree command path argument should stay allowed, got {inside_decision:?}"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn denies_bare_symlink_escape_for_command_path_argument() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "nucleus-policy-command-bare-symlink-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "nucleus-policy-command-bare-symlink-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&root).expect("worktree root should be created");
        fs::create_dir_all(&outside).expect("outside root should be created");
        fs::write(root.join("README"), "inside\n").expect("in-worktree file should be created");
        fs::write(outside.join("secret"), "outside\n").expect("outside file should be created");
        symlink(outside.join("secret"), root.join("secret")).expect("symlink should be created");

        let outside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["secret"],"cwd":"."}),
            &root,
        );
        assert!(
            matches!(outside_decision, ScopedApprovalDecision::Deny(_)),
            "bare symlinked command path argument should be denied, got {outside_decision:?}"
        );

        let inside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["README"],"cwd":"."}),
            &root,
        );
        assert!(
            matches!(inside_decision, ScopedApprovalDecision::Allow(_)),
            "bare in-worktree command path argument should stay allowed, got {inside_decision:?}"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn denies_cwd_relative_symlink_escape_for_command_path_argument() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "nucleus-policy-command-cwd-symlink-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "nucleus-policy-command-cwd-symlink-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(root.join("src")).expect("worktree src should be created");
        fs::create_dir_all(&outside).expect("outside root should be created");
        fs::write(root.join("src/README"), "inside\n").expect("in-worktree file should be created");
        fs::write(outside.join("secret"), "outside\n").expect("outside file should be created");
        symlink(outside.join("secret"), root.join("src/secret"))
            .expect("symlink should be created");

        let outside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["secret"],"cwd":"src"}),
            &root,
        );
        assert!(
            matches!(outside_decision, ScopedApprovalDecision::Deny(_)),
            "cwd-relative symlinked command path argument should be denied, got {outside_decision:?}"
        );

        let inside_decision = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"cat","args":["README"],"cwd":"src"}),
            &root,
        );
        assert!(
            matches!(inside_decision, ScopedApprovalDecision::Allow(_)),
            "cwd-relative in-worktree command path argument should stay allowed, got {inside_decision:?}"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn escalates_ambiguous_command_and_unknown_tool() {
        let ambiguous = evaluate_autonomous_approval(
            "command.run",
            &json!({"command":"date","cwd":"."}),
            worktree(),
        );
        assert!(matches!(ambiguous, ScopedApprovalDecision::Escalate(_)));

        let unknown = evaluate_autonomous_approval("custom.inspect", &json!({}), worktree());
        assert!(matches!(unknown, ScopedApprovalDecision::Escalate(_)));
    }
}
