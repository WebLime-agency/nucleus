<script lang="ts">
  import { onMount } from 'svelte';
  import { PlugZap, RefreshCw, Trash2 } from 'lucide-svelte';

  import { WorkspaceEmptyState, WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Textarea } from '$lib/components/ui/textarea';
  import { deleteMcpServer, discoverMcpServer, fetchMcpServerRecords, upsertMcpServer } from '$lib/nucleus/client';
  import type { McpServerRecord } from '$lib/nucleus/schemas';

  let servers: McpServerRecord[] = [];
  let loading = true;
  let saving = false;
  let discovering: string | null = null;
  let error: string | null = null;
  let success: string | null = null;

  let form: McpServerRecord = {
    id: '',
    workspace_id: 'workspace',
    title: '',
    transport: 'stdio',
    command: '',
    args: [],
    env_json: {},
    url: '',
    headers_json: {},
    auth_kind: 'none',
    auth_ref: '',
    enabled: true,
    sync_status: 'pending',
    last_error: '',
    last_synced_at: null,
    created_at: 0,
    updated_at: 0
  };

  function resetForm(server?: McpServerRecord) {
    form = server
      ? { ...server, args: [...server.args], env_json: structuredClone(server.env_json) }
      : {
          id: '',
          workspace_id: 'workspace',
          title: '',
          transport: 'stdio',
          command: '',
          args: [],
          env_json: {},
          url: '',
          headers_json: {},
          auth_kind: 'none',
          auth_ref: '',
          enabled: true,
          sync_status: 'pending',
          last_error: '',
          last_synced_at: null,
          created_at: 0,
          updated_at: 0
        };
  }

  function parseList(value: string) {
    return value.split(/\n|,/).map((item) => item.trim()).filter(Boolean);
  }

  function parseEnv(value: string) {
    if (!value.trim()) return {};
    return JSON.parse(value);
  }

  async function load() {
    loading = true;
    try {
      servers = await fetchMcpServerRecords();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load MCP servers.';
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    success = null;
    error = null;
    try {
      await upsertMcpServer({ ...form, env_json: parseEnv(JSON.stringify(form.env_json)) });
      success = `Saved MCP server ${form.id}.`;
      resetForm();
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save MCP server.';
    } finally {
      saving = false;
    }
  }

  async function discover(id: string) {
    discovering = id;
    try {
      await discoverMcpServer(id);
      success = `Discovery completed for ${id}.`;
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to discover MCP server.';
    } finally {
      discovering = null;
    }
  }

  async function removeServer(id: string) {
    if (!confirm(`Delete MCP server ${id}?`)) return;
    try {
      await deleteMcpServer(id);
      if (form.id === id) resetForm();
      success = `Deleted MCP server ${id}.`;
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete MCP server.';
    }
  }


  function mcpHealth(server: McpServerRecord) {
    const lastError = (server.last_error || '').toLowerCase();
    if (!server.enabled) return { label: 'Disabled', variant: 'secondary' as const, detail: '' };
    if (server.sync_status === 'missing_credentials' || lastError.includes('missing_credentials')) return { label: 'Auth missing', variant: 'destructive' as const, detail: 'Set the configured secret reference and rediscover.' };
    if (server.sync_status === 'auth_required' || lastError.includes('auth_required')) return { label: 'Auth required', variant: 'destructive' as const, detail: 'Interactive auth is required before this MCP can be used.' };
    if (server.sync_status === 'ready') return { label: 'Enabled', variant: 'default' as const, detail: '' };
    if (server.sync_status === 'pending') return { label: 'Pending', variant: 'secondary' as const, detail: '' };
    if (server.sync_status === 'unsupported_transport' || lastError.includes('unsupported_transport')) return { label: 'Unsupported', variant: 'destructive' as const, detail: server.last_error };
    if (server.sync_status === 'error') return { label: 'Error', variant: 'destructive' as const, detail: server.last_error };
    return { label: server.sync_status || 'Pending', variant: 'secondary' as const, detail: server.last_error };
  }

  onMount(load);
</script>

<svelte:head><title>Nucleus - MCPs</title></svelte:head>

<div class="space-y-8">
  <WorkspacePageHeader
    title="MCPs"
    description="Register, edit, discover, and remove daemon-backed MCP servers for workspace tool catalogs."
  />

  {#if error}<div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">{error}</div>{/if}
  {#if success}<div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">{success}</div>{/if}

  <div class="grid gap-6 xl:grid-cols-[minmax(0,1.2fr)_minmax(320px,0.8fr)]">
    <Card>
      <CardHeader>
        <CardTitle>MCP Servers</CardTitle>
        <CardDescription>Discovery syncs tool metadata into Nucleus-owned workspace state.</CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        {#if loading}
          <div class="text-sm text-zinc-400">Loading MCP servers…</div>
        {:else if servers.length === 0}
          <WorkspaceEmptyState message="No MCP servers configured yet." />
        {:else}
          {#each servers as server}
            {@const health = mcpHealth(server)}
            <div class="rounded-lg border border-zinc-800 bg-zinc-950/40 p-4">
              <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div class="min-w-0 space-y-2">
                  <div class="flex flex-wrap items-center gap-2">
                    <div class="font-medium text-zinc-50">{server.title}</div>
                    <Badge variant={health.variant}>{health.label}</Badge>
                    <Badge variant="secondary">{server.transport}</Badge>
                  </div>
                  <div class="text-xs text-zinc-500">{server.id}</div>
                  <div class="text-sm text-zinc-300 break-all">{server.transport === 'stdio' ? server.command || 'No command set.' : server.url || 'No URL set.'}</div>
                  <div class="grid gap-2 text-xs text-zinc-400 sm:grid-cols-2">
                    <div><span class="text-zinc-500">Args/Auth:</span> {server.transport === 'stdio' ? server.args.join(' ') || '—' : `${server.auth_kind}${server.auth_ref ? ` (${server.auth_ref})` : ''}`}</div>
                    <div><span class="text-zinc-500">Last synced:</span> {server.last_synced_at ?? 'Never'}</div>
                  </div>
                  {#if health.detail}
                    <div class="text-xs text-zinc-500">{health.detail}</div>
                  {/if}
                </div>
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" onclick={() => resetForm(server)}>Edit</Button>
                  <Button variant="secondary" onclick={() => discover(server.id)} disabled={discovering === server.id}>
                    <RefreshCw class={discovering === server.id ? 'size-4 animate-spin' : 'size-4'} />
                  </Button>
                  <Button variant="destructive" onclick={() => removeServer(server.id)}>
                    <Trash2 class="size-4" />
                  </Button>
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2"><PlugZap class="size-5" /> MCP Editor</CardTitle>
        <CardDescription>Create or update editable MCP server records.</CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <div class="space-y-1"><Label for="mcp-id">ID</Label><Input id="mcp-id" bind:value={form.id} /></div>
        <div class="space-y-1"><Label for="mcp-title">Title</Label><Input id="mcp-title" bind:value={form.title} /></div>
        <div class="space-y-1"><Label for="mcp-transport">Transport</Label><select id="mcp-transport" class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100" bind:value={form.transport}><option value="stdio">stdio</option><option value="streamable-http">streamable-http</option><option value="http">http</option><option value="sse">sse (unsupported)</option></select></div>
        {#if form.transport === 'stdio'}
        <div class="space-y-1"><Label for="mcp-command">Command</Label><Input id="mcp-command" bind:value={form.command} /></div>
        <div class="space-y-1"><Label for="mcp-args">Args</Label><Textarea id="mcp-args" value={form.args.join('\n')} oninput={(event) => (form.args = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div>
        <div class="space-y-1"><Label for="mcp-env">Env JSON</Label><Textarea id="mcp-env" value={JSON.stringify(form.env_json, null, 2)} oninput={(event) => (form.env_json = parseEnv((event.currentTarget as HTMLTextAreaElement).value))} rows={5} /></div>
        {:else}
        <div class="space-y-1"><Label for="mcp-url">Remote URL</Label><Input id="mcp-url" bind:value={form.url} /></div>
        <div class="space-y-1"><Label for="mcp-auth-kind">Auth mode</Label><select id="mcp-auth-kind" class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100" bind:value={form.auth_kind}><option value="none">none</option><option value="static_headers">static headers</option><option value="bearer_env">bearer from env</option><option value="oauth">oauth/device (future)</option></select></div>
        <div class="space-y-1"><Label for="mcp-auth-ref">Auth ref</Label><Input id="mcp-auth-ref" bind:value={form.auth_ref} placeholder="ENV_VAR_NAME or future secret ref" /></div>
        <div class="space-y-1"><Label for="mcp-headers">Headers JSON</Label><Textarea id="mcp-headers" value={JSON.stringify(form.headers_json, null, 2)} oninput={(event) => (form.headers_json = parseEnv((event.currentTarget as HTMLTextAreaElement).value))} rows={4} /></div>
        {/if}
        <label class="flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>
        <div class="flex gap-2">
          <Button onclick={save} disabled={saving || !form.id || !form.title}>{saving ? 'Saving…' : 'Save MCP'}</Button>
          <Button variant="secondary" onclick={() => resetForm()}>Reset</Button>
        </div>
      </CardContent>
    </Card>
  </div>
</div>
