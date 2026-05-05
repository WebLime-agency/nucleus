use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow, bail};
use nucleus_core::{AdapterKind, PRODUCT_SLUG};
use nucleus_protocol::{
    AuditEvent, ProjectSummary, RouteTarget, RouterProfileSummary, RuntimeSummary, SessionDetail,
    SessionProjectSummary, SessionSummary, SessionTurn, SessionTurnImage, StorageSummary,
    WorkspaceSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const LOCAL_AUTH_TOKEN_HASH_KEY: &str = "auth.local_token_hash";
const LOCAL_AUTH_TOKEN_FILE_NAME: &str = "local-auth-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePlan {
    pub state_dir: PathBuf,
    pub database_path: PathBuf,
    pub artifacts_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub transcripts_dir: PathBuf,
    pub playbooks_dir: PathBuf,
    pub scratch_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub route_id: String,
    pub route_title: String,
    pub scope: String,
    pub project_id: String,
    pub project_title: String,
    pub project_path: String,
    pub project_ids: Vec<String>,
    pub provider: String,
    pub model: String,
    pub working_dir: String,
    pub working_dir_kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPatch {
    pub title: Option<String>,
    pub route_id: Option<String>,
    pub route_title: Option<String>,
    pub scope: Option<String>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub project_path: Option<String>,
    pub project_ids: Option<Vec<String>>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub working_dir: Option<String>,
    pub working_dir_kind: Option<String>,
    pub state: Option<String>,
    pub provider_session_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPatch {
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEventRecord {
    pub kind: String,
    pub target: String,
    pub status: String,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub relative_path: String,
    pub absolute_path: String,
}

impl StoragePlan {
    pub fn resolve() -> Result<Self> {
        let state_dir = match env::var("NUCLEUS_STATE_DIR") {
            Ok(path) => PathBuf::from(path),
            Err(_) => default_state_dir()?,
        };

        Ok(Self::from_state_dir(state_dir))
    }

    pub fn from_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        let database_path = state_dir.join(format!("{PRODUCT_SLUG}.db"));
        let artifacts_dir = state_dir.join("artifacts");
        let memory_dir = state_dir.join("memory");
        let transcripts_dir = state_dir.join("transcripts");
        let playbooks_dir = state_dir.join("playbooks");
        let scratch_dir = state_dir.join("scratch");

        Self {
            state_dir,
            database_path,
            artifacts_dir,
            memory_dir,
            transcripts_dir,
            playbooks_dir,
            scratch_dir,
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        for path in [
            &self.state_dir,
            &self.artifacts_dir,
            &self.memory_dir,
            &self.transcripts_dir,
            &self.playbooks_dir,
            &self.scratch_dir,
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create state path {}", path.display()))?;
        }

        Ok(())
    }

    pub fn summary(&self) -> StorageSummary {
        StorageSummary {
            state_dir: display(&self.state_dir),
            database_path: display(&self.database_path),
            artifacts_dir: display(&self.artifacts_dir),
            memory_dir: display(&self.memory_dir),
            transcripts_dir: display(&self.transcripts_dir),
            playbooks_dir: display(&self.playbooks_dir),
            scratch_dir: display(&self.scratch_dir),
        }
    }
}

pub struct StateStore {
    plan: StoragePlan,
    connection: Mutex<Connection>,
}

impl StateStore {
    pub fn initialize() -> Result<Self> {
        let plan = StoragePlan::resolve()?;
        Self::initialize_with_plan(plan)
    }

    pub fn initialize_at(state_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::initialize_with_plan(StoragePlan::from_state_dir(state_dir))
    }

    fn initialize_with_plan(plan: StoragePlan) -> Result<Self> {
        plan.ensure_layout()?;

        let connection = Connection::open(&plan.database_path).with_context(|| {
            format!(
                "failed to open SQLite database at {}",
                plan.database_path.display()
            )
        })?;

        configure_connection(&connection)?;
        initialize_schema(&connection)?;
        seed_runtimes(&connection)?;
        seed_router_profiles(&connection)?;
        seed_workspace_settings(&connection)?;
        ensure_local_auth_token_with_connection(&plan, &connection)?;
        sync_projects_with_connection(&connection)?;

        Ok(Self {
            plan,
            connection: Mutex::new(connection),
        })
    }

    pub fn storage_summary(&self) -> StorageSummary {
        self.plan.summary()
    }

    pub fn local_auth_token_path(&self) -> String {
        display(&self.plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME))
    }

    pub fn read_local_auth_token(&self) -> Result<String> {
        fs::read_to_string(self.plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME))
            .with_context(|| "failed to read local auth token".to_string())
            .map(|value| value.trim().to_string())
    }

    pub fn validate_access_token(&self, token: &str) -> Result<bool> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let Some(expected_hash) = setting_value_optional(&connection, LOCAL_AUTH_TOKEN_HASH_KEY)?
        else {
            return Ok(false);
        };

        Ok(hash_auth_token(token) == expected_hash)
    }

    pub fn list_runtimes(&self) -> Result<Vec<RuntimeSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, summary, state FROM runtimes ORDER BY CASE id
                WHEN 'claude' THEN 1
                WHEN 'codex' THEN 2
                WHEN 'system' THEN 3
                ELSE 100
             END, id",
        )?;

        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let summary: String = row.get(1)?;
            let state: String = row.get(2)?;
            let kind = AdapterKind::parse(&id);

            Ok(RuntimeSummary {
                id,
                summary,
                state,
                auth_state: match kind {
                    Some(AdapterKind::System) => "not_required".to_string(),
                    _ => "unknown".to_string(),
                },
                version: String::new(),
                executable_path: String::new(),
                default_model: kind
                    .map(|adapter| adapter.default_model().to_string())
                    .unwrap_or_default(),
                note: String::new(),
                supports_sessions: kind
                    .map(|adapter| adapter.supports_sessions())
                    .unwrap_or(false),
                supports_prompting: kind
                    .map(|adapter| adapter.supports_prompting())
                    .unwrap_or(false),
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load runtimes")
    }

    pub fn workspace(&self) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn update_workspace(
        &self,
        root_path: Option<&str>,
        main_target: Option<&str>,
        utility_target: Option<&str>,
    ) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        if let Some(root_path) = root_path {
            set_workspace_root(&connection, root_path)?;
        }
        if let Some(main_target) = main_target {
            set_workspace_main_target(&connection, main_target)?;
        }
        if let Some(utility_target) = utility_target {
            set_workspace_utility_target(&connection, utility_target)?;
        }
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn sync_projects(&self) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn update_project(&self, project_id: &str, patch: ProjectPatch) -> Result<ProjectSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_project_summary(&connection, project_id)?;

        connection.execute(
            "
            UPDATE projects
            SET
                title = ?2,
                active = ?3,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![project_id, patch.title.unwrap_or(current.title), 1,],
        )?;

        load_project_summary(&connection, project_id)
    }

    pub fn resolve_project(&self, project_id: &str) -> Result<ResolvedProject> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_resolved_project(&connection, project_id)
    }

    pub fn resolve_projects(&self, project_ids: &[String]) -> Result<Vec<ResolvedProject>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        resolve_projects_with_connection(&connection, project_ids)
    }

    pub fn scratch_dir_for_session(&self, session_id: &str) -> Result<String> {
        let path = self.plan.scratch_dir.join(session_id);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create session scratch path {}", path.display()))?;
        Ok(display(&path))
    }

    pub fn list_router_profiles(&self) -> Result<Vec<RouterProfileSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_router_profiles_with_connection(&connection)
    }

    pub fn get_router_profile(&self, route_id: &str) -> Result<RouterProfileSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_router_profile(&connection, route_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_sessions_with_connection(&connection)
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_session_detail(&connection, session_id)
    }

    pub fn create_session(&self, record: SessionRecord) -> Result<SessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO sessions (
                id,
                title,
                route_id,
                route_title,
                scope,
                project_id,
                project_title,
                project_path,
                provider,
                model,
                working_dir,
                working_dir_kind,
                state,
                provider_session_id,
                last_error,
                last_message_excerpt,
                turn_count
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'active', '', '', '', 0)
            ",
            params![
                record.id,
                record.title,
                record.route_id,
                record.route_title,
                record.scope,
                record.project_id,
                record.project_title,
                record.project_path,
                record.provider,
                record.model,
                record.working_dir,
                record.working_dir_kind,
            ],
        )?;

        replace_session_projects(
            &connection,
            &record.id,
            &record.project_ids,
            &record.project_id,
        )?;

        load_session_summary(&connection, &record.id)
    }

    pub fn update_session(&self, session_id: &str, patch: SessionPatch) -> Result<SessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_session_summary(&connection, session_id)?;
        let next_title = patch.title.unwrap_or(current.title);
        let next_route_id = patch.route_id.unwrap_or(current.route_id);
        let next_route_title = patch.route_title.unwrap_or(current.route_title);
        let next_scope = patch.scope.unwrap_or(current.scope);
        let next_project_id = patch.project_id.unwrap_or(current.project_id);
        let next_project_title = patch.project_title.unwrap_or(current.project_title);
        let next_project_path = patch.project_path.unwrap_or(current.project_path);
        let next_provider = patch.provider.unwrap_or(current.provider);
        let next_model = patch.model.unwrap_or(current.model);
        let next_working_dir = patch.working_dir.unwrap_or(current.working_dir);
        let next_working_dir_kind = patch.working_dir_kind.unwrap_or(current.working_dir_kind);
        let next_state = patch.state.unwrap_or(current.state);
        let next_provider_session_id = patch
            .provider_session_id
            .unwrap_or(current.provider_session_id);
        let next_last_error = patch.last_error.unwrap_or(current.last_error);

        connection.execute(
            "
            UPDATE sessions
            SET
                title = ?2,
                route_id = ?3,
                route_title = ?4,
                scope = ?5,
                project_id = ?6,
                project_title = ?7,
                project_path = ?8,
                provider = ?9,
                model = ?10,
                working_dir = ?11,
                working_dir_kind = ?12,
                state = ?13,
                provider_session_id = ?14,
                last_error = ?15,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![
                session_id,
                next_title,
                next_route_id,
                next_route_title,
                next_scope,
                next_project_id.clone(),
                next_project_title,
                next_project_path,
                next_provider,
                next_model,
                next_working_dir,
                next_working_dir_kind,
                next_state,
                next_provider_session_id,
                next_last_error,
            ],
        )?;

        if let Some(project_ids) = patch.project_ids {
            replace_session_projects(&connection, session_id, &project_ids, &next_project_id)?;
        }

        load_session_summary(&connection, session_id)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let deleted =
            connection.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;

        if deleted == 0 {
            bail!("session '{session_id}' was not found");
        }

        Ok(())
    }

    pub fn append_session_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        role: &str,
        content: &str,
        images: &[SessionTurnImage],
    ) -> Result<SessionTurn> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_session_exists(&connection, session_id)?;
        let images_json =
            serde_json::to_string(images).context("failed to serialize session turn images")?;

        connection.execute(
            "
            INSERT INTO session_turns (id, session_id, role, content, images_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![turn_id, session_id, role, content, images_json],
        )?;

        connection.execute(
            "
            UPDATE sessions
            SET
                turn_count = turn_count + 1,
                last_message_excerpt = ?2,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![session_id, session_turn_excerpt(content, images.len())],
        )?;

        load_session_turn(&connection, turn_id)
    }

    pub fn update_session_turn_content(
        &self,
        session_id: &str,
        turn_id: &str,
        content: &str,
    ) -> Result<SessionTurn> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_session_exists(&connection, session_id)?;

        let updated = connection.execute(
            "
            UPDATE session_turns
            SET content = ?3
            WHERE id = ?1 AND session_id = ?2
            ",
            params![turn_id, session_id, content],
        )?;

        if updated == 0 {
            bail!("session turn '{turn_id}' was not found");
        }

        connection.execute(
            "
            UPDATE sessions
            SET
                last_message_excerpt = ?2,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![session_id, session_turn_excerpt(content, 0)],
        )?;

        load_session_turn(&connection, turn_id)
    }

    pub fn list_audit_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_audit_events_with_connection(&connection, limit)
    }

    pub fn append_audit_event(&self, record: AuditEventRecord) -> Result<AuditEvent> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO audit_events (kind, target, status, summary, detail)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                record.kind,
                record.target,
                record.status,
                record.summary,
                record.detail
            ],
        )?;

        let event_id = connection.last_insert_rowid();
        load_audit_event(&connection, event_id)
    }
}

fn default_state_dir() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("failed to determine home directory")?;
    Ok(home_dir.join(".nucleus"))
}

fn default_workspace_root() -> String {
    dirs::home_dir()
        .map(|path| path.join("dev-projects").display().to_string())
        .unwrap_or_else(|| "/home/eba/dev-projects".to_string())
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn session_turn_excerpt(content: &str, image_count: usize) -> String {
    if !content.trim().is_empty() {
        return excerpt(content, 160);
    }

    if image_count == 0 {
        return String::new();
    }

    if image_count == 1 {
        "[1 image]".to_string()
    } else {
        format!("[{image_count} images]")
    }
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runtimes (
            id TEXT PRIMARY KEY,
            summary TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            route_id TEXT NOT NULL DEFAULT '',
            route_title TEXT NOT NULL DEFAULT '',
            scope TEXT NOT NULL DEFAULT 'ad_hoc',
            project_id TEXT NOT NULL DEFAULT '',
            project_title TEXT NOT NULL DEFAULT '',
            project_path TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL,
            model TEXT NOT NULL DEFAULT '',
            working_dir TEXT NOT NULL DEFAULT '',
            working_dir_kind TEXT NOT NULL DEFAULT 'workspace_scratch',
            state TEXT NOT NULL,
            provider_session_id TEXT NOT NULL DEFAULT '',
            last_error TEXT NOT NULL DEFAULT '',
            last_message_excerpt TEXT NOT NULL DEFAULT '',
            turn_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS session_turns (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            images_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_created_at
            ON session_turns(session_id, created_at, id);

        CREATE TABLE IF NOT EXISTS session_projects (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            sort_order INTEGER NOT NULL DEFAULT 0,
            is_primary INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY (session_id, project_id)
        );

        CREATE INDEX IF NOT EXISTS idx_session_projects_session_id_sort_order
            ON session_projects(session_id, sort_order, project_id);

        CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            target TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'info',
            summary TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            slug TEXT NOT NULL,
            relative_path TEXT NOT NULL UNIQUE,
            absolute_path TEXT NOT NULL UNIQUE,
            active INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS router_profiles (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            summary TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            targets_json TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        ",
    )?;

    ensure_column(
        connection,
        "sessions",
        "route_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "route_title",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "scope",
        "TEXT NOT NULL DEFAULT 'ad_hoc'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_title",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(connection, "sessions", "model", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        connection,
        "sessions",
        "working_dir",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "working_dir_kind",
        "TEXT NOT NULL DEFAULT 'workspace_scratch'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "provider_session_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "last_error",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "last_message_excerpt",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "turn_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "target",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "status",
        "TEXT NOT NULL DEFAULT 'info'",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "summary",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "session_turns",
        "images_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;

    Ok(())
}

fn seed_runtimes(connection: &Connection) -> Result<()> {
    for adapter in AdapterKind::ALL {
        connection.execute(
            "
            INSERT INTO runtimes (id, summary, state)
            VALUES (?1, ?2, 'planned')
            ON CONFLICT(id) DO UPDATE SET
                summary = excluded.summary,
                updated_at = unixepoch()
            ",
            params![adapter.as_str(), adapter.summary()],
        )?;
    }

    Ok(())
}

fn seed_router_profiles(connection: &Connection) -> Result<()> {
    let profiles = [
        (
            "claude-sonnet",
            "Claude Sonnet",
            "Direct Claude Sonnet route through the local Claude runtime.",
            true,
            json!([{ "provider": "claude", "model": "sonnet" }]),
        ),
        (
            "codex-default",
            "Codex Default",
            "Direct Codex route through the local Codex runtime default model.",
            true,
            json!([{ "provider": "codex", "model": "" }]),
        ),
        (
            "balanced",
            "Balanced",
            "Prefer Claude Sonnet first, then fall back to Codex when needed.",
            true,
            json!([
                { "provider": "claude", "model": "sonnet" },
                { "provider": "codex", "model": "gpt-5.4" }
            ]),
        ),
    ];

    for (id, title, summary, enabled, targets) in profiles {
        connection.execute(
            "
            INSERT INTO router_profiles (id, title, summary, enabled, targets_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                summary = excluded.summary,
                enabled = excluded.enabled,
                targets_json = excluded.targets_json,
                updated_at = unixepoch()
            ",
            params![id, title, summary, enabled as i64, targets.to_string()],
        )?;
    }

    Ok(())
}

fn seed_workspace_settings(connection: &Connection) -> Result<()> {
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_root', ?1)
        ON CONFLICT(key) DO NOTHING
        ",
        params![default_workspace_root()],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_main_target', 'route:balanced')
        ON CONFLICT(key) DO NOTHING
        ",
        [],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_utility_target', 'route:codex-default')
        ON CONFLICT(key) DO NOTHING
        ",
        [],
    )?;

    Ok(())
}

fn ensure_local_auth_token_with_connection(plan: &StoragePlan, connection: &Connection) -> Result<()> {
    let token_path = plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME);
    let token_value = match setting_value_optional(connection, LOCAL_AUTH_TOKEN_HASH_KEY)? {
        Some(hash) => {
            let file_token = read_token_file(&token_path)?;

            if !file_token.is_empty() && hash_auth_token(&file_token) == hash {
                file_token
            } else {
                let next = generate_local_auth_token();
                write_token_file(&token_path, &next)?;
                set_setting_value(connection, LOCAL_AUTH_TOKEN_HASH_KEY, &hash_auth_token(&next))?;
                next
            }
        }
        None => {
            let next = generate_local_auth_token();
            write_token_file(&token_path, &next)?;
            set_setting_value(connection, LOCAL_AUTH_TOKEN_HASH_KEY, &hash_auth_token(&next))?;
            next
        }
    };

    if !token_value.is_empty() {
        write_token_file(&token_path, &token_value)?;
    }

    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let existing = table_columns(connection, table)?;

    if existing.contains(column) {
        return Ok(());
    }

    connection.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;

    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    let columns = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to inspect table columns")?;

    Ok(columns.into_iter().collect())
}

fn load_workspace_summary(connection: &Connection) -> Result<WorkspaceSummary> {
    Ok(WorkspaceSummary {
        root_path: workspace_root(connection)?,
        main_target: workspace_main_target(connection)?,
        utility_target: workspace_utility_target(connection)?,
        projects: list_projects_with_connection(connection)?,
    })
}

fn workspace_root(connection: &Connection) -> Result<String> {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'workspace_root'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("workspace root is not configured"))
}

fn set_workspace_root(connection: &Connection, root_path: &str) -> Result<()> {
    set_setting_value(connection, "workspace_root", root_path)?;
    Ok(())
}

fn workspace_main_target(connection: &Connection) -> Result<String> {
    setting_value(connection, "workspace_main_target")
}

fn set_workspace_main_target(connection: &Connection, target: &str) -> Result<()> {
    set_setting_value(connection, "workspace_main_target", target)
}

fn workspace_utility_target(connection: &Connection) -> Result<String> {
    setting_value(connection, "workspace_utility_target")
}

fn set_workspace_utility_target(connection: &Connection, target: &str) -> Result<()> {
    set_setting_value(connection, "workspace_utility_target", target)
}

fn setting_value(connection: &Connection, key: &str) -> Result<String> {
    setting_value_optional(connection, key)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("setting '{key}' is not configured"))
}

fn setting_value_optional(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load setting")
}

fn set_setting_value(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute(
        "
        INSERT INTO app_settings (key, value, updated_at)
        VALUES (?1, ?2, unixepoch())
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at
        ",
        params![key, value],
    )?;
    Ok(())
}

fn generate_local_auth_token() -> String {
    format!(
        "nuctk_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn hash_auth_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);

    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }

    output
}

fn read_token_file(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }

    fs::read_to_string(path)
        .with_context(|| format!("failed to read token file '{}'", path.display()))
        .map(|value| value.trim().to_string())
}

fn write_token_file(path: &Path, token: &str) -> Result<()> {
    fs::write(path, format!("{token}\n"))
        .with_context(|| format!("failed to write token file '{}'", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions).with_context(|| {
            format!(
                "failed to set permissions on token file '{}'",
                path.display()
            )
        })?;
    }

    Ok(())
}

fn sync_projects_with_connection(connection: &Connection) -> Result<()> {
    let root_path = workspace_root(connection)?;
    let root = PathBuf::from(&root_path);

    if !root.is_dir() {
        bail!("workspace root '{}' does not exist", root.display());
    }

    for project in discover_projects(&root)? {
        connection.execute(
            "
            INSERT INTO projects (id, title, slug, relative_path, absolute_path, active)
            VALUES (?1, ?2, ?3, ?4, ?5, 1)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                slug = excluded.slug,
                relative_path = excluded.relative_path,
                absolute_path = excluded.absolute_path,
                updated_at = unixepoch()
            ",
            params![
                project.id,
                project.title,
                project.slug,
                project.relative_path,
                project.absolute_path
            ],
        )?;
    }

    backfill_legacy_session_projects_with_connection(connection)?;

    Ok(())
}

fn list_projects_with_connection(connection: &Connection) -> Result<Vec<ProjectSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, slug, relative_path, absolute_path, created_at, updated_at
        FROM projects
        ORDER BY relative_path ASC, title ASC
        ",
    )?;

    let rows = statement.query_map([], map_project_summary)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load projects")
}

fn load_project_summary(connection: &Connection, project_id: &str) -> Result<ProjectSummary> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path, created_at, updated_at
            FROM projects
            WHERE id = ?1
            ",
            params![project_id],
            map_project_summary,
        )
        .optional()?
        .ok_or_else(|| anyhow!("project '{project_id}' was not found"))
}

fn map_project_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectSummary> {
    Ok(ProjectSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        slug: row.get(2)?,
        relative_path: row.get(3)?,
        absolute_path: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn load_resolved_project(connection: &Connection, project_id: &str) -> Result<ResolvedProject> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path
            FROM projects
            WHERE id = ?1
            ",
            params![project_id],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("project '{project_id}' was not found"))
}

fn load_resolved_project_by_path(
    connection: &Connection,
    path: &str,
) -> Result<Option<ResolvedProject>> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path
            FROM projects
            WHERE absolute_path = ?1 OR relative_path = ?1
            ORDER BY CASE
                WHEN absolute_path = ?1 THEN 0
                ELSE 1
            END
            LIMIT 1
            ",
            params![path],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()
        .context("failed to resolve project by path")
}

fn resolve_projects_with_connection(
    connection: &Connection,
    project_ids: &[String],
) -> Result<Vec<ResolvedProject>> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for project_id in project_ids {
        let project_id = project_id.trim();

        if project_id.is_empty() || !seen.insert(project_id.to_string()) {
            continue;
        }

        resolved.push(load_resolved_project(connection, project_id)?);
    }

    Ok(resolved)
}

fn backfill_legacy_session_projects_with_connection(connection: &Connection) -> Result<()> {
    #[derive(Debug)]
    struct LegacySessionProjectRecord {
        session_id: String,
        project_id: String,
        project_path: String,
        working_dir: String,
        working_dir_kind: String,
    }

    let mut statement = connection.prepare(
        "
        SELECT id, project_id, project_path, scope, working_dir, working_dir_kind
        FROM sessions
        WHERE TRIM(project_id) <> ''
           OR EXISTS (
                SELECT 1
                FROM session_projects
                WHERE session_projects.session_id = sessions.id
           )
        ",
    )?;

    let rows = statement.query_map([], |row| {
        Ok(LegacySessionProjectRecord {
            session_id: row.get(0)?,
            project_id: row.get(1)?,
            project_path: row.get(2)?,
            working_dir: row.get(4)?,
            working_dir_kind: row.get(5)?,
        })
    })?;

    let records = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to inspect legacy session project rows")?;
    drop(statement);

    for record in records {
        let existing_count = connection.query_row(
            "SELECT COUNT(*) FROM session_projects WHERE session_id = ?1",
            params![record.session_id],
            |row| row.get::<_, i64>(0),
        )?;

        let project = if existing_count > 0 {
            load_primary_project_for_session(connection, &record.session_id)?.or(
                resolve_legacy_session_project(
                    connection,
                    record.project_id.trim(),
                    record.project_path.trim(),
                )?,
            )
        } else {
            let project = resolve_legacy_session_project(
                connection,
                record.project_id.trim(),
                record.project_path.trim(),
            )?;

            if let Some(project) = project.as_ref() {
                connection.execute(
                    "
                    INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
                    VALUES (?1, ?2, 0, 1)
                    ON CONFLICT(session_id, project_id) DO UPDATE SET
                        sort_order = excluded.sort_order,
                        is_primary = excluded.is_primary
                    ",
                    params![record.session_id, project.id],
                )?;
            }

            project
        };
        let Some(project) = project else {
            continue;
        };

        let next_working_dir = if record.working_dir.trim().is_empty()
            || record.working_dir_kind == "workspace_scratch"
        {
            project.absolute_path.clone()
        } else {
            record.working_dir
        };
        let next_working_dir_kind = if record.working_dir_kind.trim().is_empty()
            || record.working_dir_kind == "workspace_scratch"
            || record.working_dir_kind == "project"
        {
            "project_root".to_string()
        } else {
            record.working_dir_kind
        };
        let next_scope = match existing_count.max(1) {
            1 => "project".to_string(),
            _ => "multi_project".to_string(),
        };

        connection.execute(
            "
            UPDATE sessions
            SET
                scope = ?2,
                project_id = ?3,
                project_title = ?4,
                project_path = ?5,
                working_dir = ?6,
                working_dir_kind = ?7
            WHERE id = ?1
            ",
            params![
                record.session_id,
                next_scope,
                project.id,
                project.title,
                project.absolute_path,
                next_working_dir,
                next_working_dir_kind,
            ],
        )?;
    }

    Ok(())
}

fn resolve_legacy_session_project(
    connection: &Connection,
    project_id: &str,
    project_path: &str,
) -> Result<Option<ResolvedProject>> {
    if !project_id.is_empty() {
        if let Ok(project) = load_resolved_project(connection, project_id) {
            return Ok(Some(project));
        }
    }

    if !project_path.is_empty() {
        if let Some(project) = load_resolved_project_by_path(connection, project_path)? {
            return Ok(Some(project));
        }
    }

    Ok(None)
}

fn load_primary_project_for_session(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<ResolvedProject>> {
    connection
        .query_row(
            "
            SELECT
                projects.id,
                projects.title,
                projects.slug,
                projects.relative_path,
                projects.absolute_path
            FROM session_projects
            INNER JOIN projects ON projects.id = session_projects.project_id
            WHERE session_projects.session_id = ?1
            ORDER BY session_projects.is_primary DESC, session_projects.sort_order ASC, projects.relative_path ASC
            LIMIT 1
            ",
            params![session_id],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()
        .context("failed to load primary session project")
}

fn replace_session_projects(
    connection: &Connection,
    session_id: &str,
    project_ids: &[String],
    primary_project_id: &str,
) -> Result<()> {
    connection.execute(
        "DELETE FROM session_projects WHERE session_id = ?1",
        params![session_id],
    )?;

    for (index, project_id) in project_ids.iter().enumerate() {
        connection.execute(
            "
            INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                session_id,
                project_id,
                index as i64,
                (project_id == primary_project_id) as i64,
            ],
        )?;
    }

    Ok(())
}

fn list_router_profiles_with_connection(
    connection: &Connection,
) -> Result<Vec<RouterProfileSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, summary, enabled, targets_json
        FROM router_profiles
        ORDER BY CASE id
            WHEN 'balanced' THEN 1
            WHEN 'claude-sonnet' THEN 2
            WHEN 'codex-default' THEN 3
            ELSE 100
        END, title ASC
        ",
    )?;

    let rows = statement.query_map([], map_router_profile)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load router profiles")
}

fn load_router_profile(connection: &Connection, route_id: &str) -> Result<RouterProfileSummary> {
    connection
        .query_row(
            "
            SELECT id, title, summary, enabled, targets_json
            FROM router_profiles
            WHERE id = ?1
            ",
            params![route_id],
            map_router_profile,
        )
        .optional()?
        .ok_or_else(|| anyhow!("router profile '{route_id}' was not found"))
}

fn map_router_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouterProfileSummary> {
    let targets_json: String = row.get(4)?;
    let targets = serde_json::from_str::<Vec<RouteTarget>>(&targets_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;

    Ok(RouterProfileSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        summary: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        state: "configured".to_string(),
        targets,
    })
}

fn list_sessions_with_connection(connection: &Connection) -> Result<Vec<SessionSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM sessions
        ORDER BY updated_at DESC, created_at DESC, id DESC
        LIMIT 50
        ",
    )?;

    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session identifiers")?;

    ids.into_iter()
        .map(|session_id| load_session_summary(connection, &session_id))
        .collect()
}

fn load_session_summary(connection: &Connection, session_id: &str) -> Result<SessionSummary> {
    let mut session = connection
        .query_row(
            "
            SELECT
                id,
                title,
                route_id,
                route_title,
                scope,
                project_id,
                project_title,
                project_path,
                provider,
                model,
                working_dir,
                working_dir_kind,
                state,
                provider_session_id,
                last_error,
                last_message_excerpt,
                turn_count,
                created_at,
                updated_at
            FROM sessions
            WHERE id = ?1
            ",
            params![session_id],
            map_session_summary_row,
        )
        .optional()?
        .ok_or_else(|| anyhow!("session '{session_id}' was not found"))?;

    session.projects = load_session_projects(connection, session_id)?;
    session.project_count = session.projects.len();
    session.scope = session_scope_from_projects(&session.projects, &session.scope);

    if session.project_id.is_empty() {
        if let Some(primary) = session.projects.iter().find(|project| project.is_primary) {
            session.project_id = primary.id.clone();
            session.project_title = primary.title.clone();
            session.project_path = primary.absolute_path.clone();
        }
    }

    Ok(session)
}

fn map_session_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        route_id: row.get(2)?,
        route_title: row.get(3)?,
        scope: row.get(4)?,
        project_id: row.get(5)?,
        project_title: row.get(6)?,
        project_path: row.get(7)?,
        provider: row.get(8)?,
        model: row.get(9)?,
        working_dir: row.get(10)?,
        working_dir_kind: row.get(11)?,
        project_count: 0,
        projects: Vec::new(),
        state: row.get(12)?,
        provider_session_id: row.get(13)?,
        last_error: row.get(14)?,
        last_message_excerpt: row.get(15)?,
        turn_count: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

fn load_session_detail(connection: &Connection, session_id: &str) -> Result<SessionDetail> {
    let session = load_session_summary(connection, session_id)?;
    let mut statement = connection.prepare(
        "
        SELECT id, session_id, role, content, images_json, created_at
        FROM session_turns
        WHERE session_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;

    let rows = statement.query_map(params![session_id], |row| {
        Ok(SessionTurn {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            images: decode_session_turn_images(row.get::<_, String>(4)?)?,
            created_at: row.get(5)?,
        })
    })?;

    let turns = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session turns")?;

    Ok(SessionDetail { session, turns })
}

fn load_session_turn(connection: &Connection, turn_id: &str) -> Result<SessionTurn> {
    connection
        .query_row(
            "
            SELECT id, session_id, role, content, images_json, created_at
            FROM session_turns
            WHERE id = ?1
            ",
            params![turn_id],
            |row| {
                Ok(SessionTurn {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    images: decode_session_turn_images(row.get::<_, String>(4)?)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("session turn '{turn_id}' was not found"))
}

fn decode_session_turn_images(value: String) -> rusqlite::Result<Vec<SessionTurnImage>> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn load_session_projects(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<SessionProjectSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT
            projects.id,
            projects.title,
            projects.slug,
            projects.relative_path,
            projects.absolute_path,
            session_projects.is_primary
        FROM session_projects
        INNER JOIN projects ON projects.id = session_projects.project_id
        WHERE session_projects.session_id = ?1
        ORDER BY session_projects.is_primary DESC, session_projects.sort_order ASC, projects.relative_path ASC
        ",
    )?;

    let rows = statement.query_map(params![session_id], |row| {
        Ok(SessionProjectSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            slug: row.get(2)?,
            relative_path: row.get(3)?,
            absolute_path: row.get(4)?,
            is_primary: row.get::<_, i64>(5)? != 0,
        })
    })?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session projects")
}

fn session_scope_from_projects(projects: &[SessionProjectSummary], stored_scope: &str) -> String {
    if projects.is_empty() {
        if stored_scope.trim().is_empty() {
            "ad_hoc".to_string()
        } else {
            stored_scope.to_string()
        }
    } else if projects.len() == 1 {
        "project".to_string()
    } else {
        "multi_project".to_string()
    }
}

fn list_audit_events_with_connection(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AuditEvent>> {
    let mut statement = connection.prepare(
        "
        SELECT id, kind, target, status, summary, detail, created_at
        FROM audit_events
        ORDER BY id DESC
        LIMIT ?1
        ",
    )?;

    let rows = statement.query_map(params![limit as i64], map_audit_event)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load audit events")
}

fn load_audit_event(connection: &Connection, event_id: i64) -> Result<AuditEvent> {
    connection
        .query_row(
            "
            SELECT id, kind, target, status, summary, detail, created_at
            FROM audit_events
            WHERE id = ?1
            ",
            params![event_id],
            map_audit_event,
        )
        .optional()?
        .ok_or_else(|| anyhow!("audit event '{event_id}' was not found"))
}

fn map_audit_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    Ok(AuditEvent {
        id: row.get(0)?,
        kind: row.get(1)?,
        target: row.get(2)?,
        status: row.get(3)?,
        summary: row.get(4)?,
        detail: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn ensure_session_exists(connection: &Connection, session_id: &str) -> Result<()> {
    let found = connection
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if found.is_none() {
        bail!("session '{session_id}' was not found");
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredProject {
    id: String,
    title: String,
    slug: String,
    relative_path: String,
    absolute_path: String,
}

fn discover_projects(root: &Path) -> Result<Vec<DiscoveredProject>> {
    let mut projects = Vec::new();
    discover_projects_recursive(root, root, 0, &mut projects)?;
    projects.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    projects.dedup_by(|left, right| left.relative_path == right.relative_path);
    Ok(projects)
}

fn discover_projects_recursive(
    root: &Path,
    current: &Path,
    depth: usize,
    projects: &mut Vec<DiscoveredProject>,
) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }

    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(anyhow!(
                "failed to read workspace directory '{}': {error}",
                current.display()
            ));
        }
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !path.is_dir() || is_ignored_project_dir(&name) {
            continue;
        }

        if is_project_dir(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let title = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| relative.clone());
            let slug = slugify(&relative);

            projects.push(DiscoveredProject {
                id: slug.clone(),
                title,
                slug,
                relative_path: relative,
                absolute_path: path.display().to_string(),
            });
            continue;
        }

        discover_projects_recursive(root, &path, depth + 1, projects)?;
    }

    Ok(())
}

fn is_project_dir(path: &Path) -> bool {
    [
        ".git",
        "package.json",
        "Cargo.toml",
        "AGENTS.md",
        ".a0proj",
        "README.md",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

fn is_ignored_project_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "build" | "dist" | ".svelte-kit"
    )
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for character in value.chars() {
        let lower = character.to_ascii_lowercase();

        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn persists_and_lists_audit_events() {
        let state_dir = test_state_dir("audit-events");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let first = store
            .append_audit_event(AuditEventRecord {
                kind: "session.created".to_string(),
                target: "session:test-1".to_string(),
                status: "success".to_string(),
                summary: "Created a test session.".to_string(),
                detail: "provider=claude model=sonnet".to_string(),
            })
            .expect("first audit event should persist");
        let second = store
            .append_audit_event(AuditEventRecord {
                kind: "action.executed".to_string(),
                target: "action:runtime.refresh".to_string(),
                status: "success".to_string(),
                summary: "Refreshed runtime health.".to_string(),
                detail: "count=4".to_string(),
            })
            .expect("second audit event should persist");

        let recent = store
            .list_audit_events(10)
            .expect("audit events should load");

        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, second.id);
        assert_eq!(recent[0].summary, "Refreshed runtime health.");
        assert_eq!(recent[1].id, first.id);
        assert_eq!(recent[1].target, "session:test-1");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn resolves_discovered_projects_without_global_activation() {
        let state_dir = test_state_dir("resolve-project");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
            )
            .expect("workspace root should update");
        let project_id = workspace
            .projects
            .first()
            .expect("a discovered project should exist")
            .id
            .clone();

        let project = store
            .resolve_project(&project_id)
            .expect("discovered project should resolve");
        assert_eq!(project.id, project_id);
        assert_eq!(project.relative_path, "alpha");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn backfills_legacy_project_sessions_into_session_projects() {
        let state_dir = test_state_dir("legacy-session-backfill");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
            )
            .expect("workspace root should update");
        let project = workspace
            .projects
            .iter()
            .find(|project| project.relative_path == "alpha")
            .expect("alpha project should be discovered")
            .clone();
        drop(store);

        let plan = StoragePlan::from_state_dir(&state_dir);
        let connection =
            Connection::open(&plan.database_path).expect("legacy database should open");
        connection
            .execute(
                "
                INSERT INTO sessions (
                    id,
                    title,
                    scope,
                    project_id,
                    project_title,
                    project_path,
                    provider,
                    model,
                    working_dir,
                    working_dir_kind,
                    state
                )
                VALUES (?1, ?2, 'project', ?3, ?4, ?5, 'codex', 'gpt-5.4', '', 'workspace_scratch', 'active')
                ",
                params![
                    "legacy-session-1",
                    "Legacy session",
                    project.id,
                    project.title,
                    project.absolute_path,
                ],
            )
            .expect("legacy session row should insert");
        drop(connection);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let session = store
            .get_session("legacy-session-1")
            .expect("legacy session should load")
            .session;

        assert_eq!(session.scope, "project");
        assert_eq!(session.project_count, 1);
        assert_eq!(session.working_dir_kind, "project_root");
        assert_eq!(session.working_dir, project.absolute_path);
        assert_eq!(session.project_id, project.id);
        assert_eq!(session.projects.len(), 1);
        assert_eq!(session.projects[0].id, project.id);
        assert!(session.projects[0].is_primary);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn normalizes_existing_project_session_metadata_on_startup() {
        let state_dir = test_state_dir("project-session-normalization");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
            )
            .expect("workspace root should update");
        let project = workspace
            .projects
            .iter()
            .find(|project| project.relative_path == "alpha")
            .expect("alpha project should be discovered")
            .clone();
        drop(store);

        let plan = StoragePlan::from_state_dir(&state_dir);
        let connection =
            Connection::open(&plan.database_path).expect("legacy database should open");
        connection
            .execute(
                "
                INSERT INTO sessions (
                    id,
                    title,
                    scope,
                    project_id,
                    project_title,
                    project_path,
                    provider,
                    model,
                    working_dir,
                    working_dir_kind,
                    state
                )
                VALUES (?1, ?2, 'project', ?3, ?4, ?5, 'codex', 'gpt-5.4', ?5, 'project', 'active')
                ",
                params![
                    "legacy-session-2",
                    "Legacy normalized session",
                    project.id,
                    project.title,
                    project.absolute_path,
                ],
            )
            .expect("legacy session row should insert");
        connection
            .execute(
                "
                INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
                VALUES (?1, ?2, 0, 1)
                ",
                params!["legacy-session-2", project.id],
            )
            .expect("legacy session project row should insert");
        drop(connection);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let session = store
            .get_session("legacy-session-2")
            .expect("legacy session should load")
            .session;

        assert_eq!(session.scope, "project");
        assert_eq!(session.working_dir_kind, "project_root");
        assert_eq!(session.working_dir, project.absolute_path);
        assert_eq!(session.project_count, 1);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn updates_existing_session_turn_content() {
        let state_dir = test_state_dir("update-session-turn");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let scratch_dir = store
            .scratch_dir_for_session("stream-session")
            .expect("scratch dir should resolve");

        store
            .create_session(SessionRecord {
                id: "stream-session".to_string(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                title: "Streaming session".to_string(),
                provider: "codex".to_string(),
                model: "gpt-5.4".to_string(),
                working_dir: scratch_dir,
                working_dir_kind: "workspace_scratch".to_string(),
            })
            .expect("session should persist");
        store
            .append_session_turn("stream-session", "user-1", "user", "Hello", &[])
            .expect("user turn should append");
        store
            .append_session_turn("stream-session", "assistant-1", "assistant", "Hi", &[])
            .expect("assistant turn should append");

        let turn = store
            .update_session_turn_content("stream-session", "assistant-1", "Hi there")
            .expect("assistant turn should update");
        let session = store
            .get_session("stream-session")
            .expect("session should reload");

        assert_eq!(turn.content, "Hi there");
        assert_eq!(
            session
                .turns
                .iter()
                .find(|turn| turn.id == "assistant-1")
                .expect("assistant turn should exist")
                .content,
            "Hi there"
        );
        assert_eq!(session.session.last_message_excerpt, "Hi there");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn provisions_and_validates_local_auth_token() {
        let state_dir = test_state_dir("local-auth-token");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let token_path = PathBuf::from(store.local_auth_token_path());
        let token = store
            .read_local_auth_token()
            .expect("local auth token should exist");

        assert!(token_path.is_file());
        assert!(token.starts_with("nuctk_"));
        assert!(
            store
                .validate_access_token(&token)
                .expect("token validation should succeed")
        );
        assert!(
            !store
                .validate_access_token("nuctk_invalid")
                .expect("invalid token check should succeed")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }
}
