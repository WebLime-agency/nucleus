# Native MCP support

Nucleus MCP records support local `stdio` servers and native remote `streamable-http` / `http` servers. `sse` is reserved and currently returns `unsupported_transport`.

Config fields: `transport`, stdio `command`/`args`/`env_json`, remote `url`, safe `headers_json`, `auth_kind`, `auth_ref`, `sync_status`, `last_error`, `last_synced_at`, `tools_json`, and `resources_json`.

Auth modes are `none`, `static_headers`, `bearer_env`/`env_bearer`, and future `oauth`/`device`. Bearer secrets are resolved from an environment variable named by `auth_ref`; raw secrets must not be stored in prompt includes or exposed by API responses. Sensitive headers such as Authorization, cookies, tokens and API keys are redacted in summaries.

Discovery initializes the MCP server, sends the initialized notification, then runs `tools/list`. Tool invocation initializes and calls `tools/call` through the same native transport path. Remote HTTP responses may be JSON or SSE-style `data:` frames.

Bridge conversion: run `scripts/convert-mcp-bridges.py /path/to/nucleus.db`. It recognizes `transport=stdio`, `command=npx`, and args containing `mcp-remote <url>`, then converts the record to native `streamable-http`, clears the bridge command/args, preserves title/enabled/catalog metadata, and assigns provider auth status.

Statuses: `pending` awaits discovery; `ready` discovered successfully; `missing_credentials` needs a configured secret reference; `auth_required` needs unsupported interactive auth; `unsupported_transport` is not implemented; `error` is a non-secret failure summary.
