#!/usr/bin/env python3
import json, sqlite3, sys
from pathlib import Path

def auth_for(id_, url):
    unauth = {"cloudflare-docs", "context7", "emdash-docs"}
    if id_ in unauth:
        return "none", ""
    if id_.startswith("cloudflare"):
        return "vault_bearer", "vault://workspace/CLOUDFLARE_API_TOKEN"
    if id_ == "supabase":
        return "vault_bearer", "vault://workspace/SUPABASE_ACCESS_TOKEN"
    if id_ == "vercel":
        return "vault_bearer", "vault://workspace/VERCEL_TOKEN"
    return "oauth", ""

def main():
    db = Path(sys.argv[1] if len(sys.argv) > 1 else "/home/eba/.nucleus-eba/nucleus.db")
    con = sqlite3.connect(db)
    cur = con.cursor()
    for col, ddl in [
        ("url", "TEXT NOT NULL DEFAULT ''"),
        ("headers_json", "TEXT NOT NULL DEFAULT '{}'"),
        ("auth_kind", "TEXT NOT NULL DEFAULT 'none'"),
        ("auth_ref", "TEXT NOT NULL DEFAULT ''"),
    ]:
        cols = {r[1] for r in cur.execute("pragma table_info(mcp_servers)")}
        if col not in cols:
            cur.execute(f"alter table mcp_servers add column {col} {ddl}")
    rows = cur.execute("select id,args_json from mcp_servers where transport='stdio' and command='npx'").fetchall()
    changed = []
    for id_, args_json in rows:
        try: args = json.loads(args_json or '[]')
        except Exception: continue
        if "mcp-remote" not in args: continue
        idx = args.index("mcp-remote")
        url = next((a for a in args[idx+1:] if isinstance(a,str) and a.startswith(('http://','https://'))), '')
        if not url: continue
        auth_kind, auth_ref = auth_for(id_, url)
        cur.execute("""
          update mcp_servers
          set transport='streamable-http', url=?, command='', args_json='[]',
              auth_kind=?, auth_ref=?, headers_json=coalesce(nullif(headers_json,''),'{}'),
              sync_status=case when ?='none' then 'pending' else 'missing_credentials' end,
              last_error=case when ?='none' then '' else 'missing_credentials: configure native MCP credentials' end,
              updated_at=unixepoch()
          where id=?
        """, (url, auth_kind, auth_ref, auth_kind, auth_kind, id_))
        changed.append((id_, url, auth_kind, auth_ref))
    con.commit()
    for row in changed:
        print("converted", *row)
    print(f"converted_count={len(changed)}")
if __name__ == '__main__': main()
