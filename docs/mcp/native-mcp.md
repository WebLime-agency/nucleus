# Native MCP support

Nucleus MCP records support local `stdio` servers and native remote `streamable-http` / `http` servers. `sse` is reserved and currently returns `unsupported_transport`.

Config fields: `transport`, stdio `command`/`args`/`env_json`, remote `url`, safe `headers_json`, `auth_kind`, `auth_ref`, `sync_status`, `last_error`, `last_synced_at`, `tools_json`, and `resources_json`.

Auth modes are `none`, `static_headers`, `vault_bearer`, and future `oauth`/`device`. Bearer-token MCP auth must use Vault; daemon process environment variables are not a supported bearer credential path. Existing records with legacy `bearer_env`/`env_bearer` fail closed with `auth_migration_required` until the token is moved into Vault. Raw secrets must not be stored in prompt includes or exposed by API responses. Sensitive headers such as Authorization, cookies, tokens and API keys are redacted in summaries.

Recommended Vault bearer setup:

1. Create a Workspace or Project Vault secret, for example `CLOUDFLARE_API_TOKEN`.
2. Configure the MCP auth mode as `vault_bearer`.
3. In the normal UI flow, select the Vault scope and enter or select the secret name. Nucleus stores the durable `auth_ref` as `vault://workspace/<secret_name>` or `vault://project/<project_id>/<secret_name>`.
4. Grant the MCP server an allowed-consumer policy with `consumer_kind=mcp`, `permission=read`, and `approval_mode=allow`.

Discovery initializes the MCP server, sends the initialized notification, then runs `tools/list`. Tool invocation initializes and calls `tools/call` through the same native transport path. Remote HTTP responses may be JSON or SSE-style `data:` frames.

Bridge conversion: run `scripts/convert-mcp-bridges.py /path/to/nucleus.db`. It recognizes `transport=stdio`, `command=npx`, and args containing `mcp-remote <url>`, then converts the record to native `streamable-http`, clears the bridge command/args, preserves title/enabled/catalog metadata, and assigns provider auth status.

Statuses: `pending` awaits discovery; `ready` discovered successfully; `auth_migration_required` means a legacy env-bearer record must move to Vault; `missing_credentials` needs a configured secret reference; `vault_locked`, `vault_secret_missing`, `vault_policy_denied`, `vault_project_context_missing`, and `vault_project_context_mismatch` describe Vault bearer setup or scope failures; `auth_required` needs unsupported interactive auth; `unsupported_transport` is not implemented; `error` is a non-secret failure summary.
