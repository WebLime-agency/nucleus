<script lang="ts">
  import { onMount } from 'svelte';
  import {
    Check,
    Download,
    GitBranch,
    HardDrive,
    KeyRound,
    Link,
    RefreshCcw,
    Settings2,
    Server,
    ShieldAlert
  } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { applyUpdate, checkForUpdates, fetchSettings } from '$lib/nucleus/client';
  import { compactPath, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, SettingsSummary } from '$lib/nucleus/schemas';

  let settings = $state<SettingsSummary | null>(null);
  let loading = $state(true);
  let checking = $state(false);
  let applying = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let update = $derived(settings?.update ?? null);
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (checking) return 'Checking';
    if (applying) return 'Updating';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (error) return 'Degraded';
    return 'Live';
  });
  let canCheck = $derived(
    !!settings && settings.update.install_mode === 'git' && !checking && !applying
  );
  let canApply = $derived(
    !!update &&
      update.install_mode === 'git' &&
      update.update_available &&
      !update.dirty_worktree &&
      !update.restart_required &&
      !checking &&
      !applying
  );

  function updateStateVariant(
    value: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (value === 'ready') return 'default';
    if (value === 'checking' || value === 'applying') return 'warning';
    if (value === 'unsupported' || value === 'idle') return 'secondary';
    return 'destructive';
  }

  async function loadSettings() {
    loading = settings === null;

    try {
      settings = await fetchSettings();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load settings.';
    } finally {
      loading = false;
    }
  }

  async function handleCheckForUpdates() {
    checking = true;
    success = null;

    try {
      const next = await checkForUpdates();
      settings = settings ? { ...settings, update: next } : settings;
      error = null;
      success = next.message;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to check for updates.';
    } finally {
      checking = false;
    }
  }

  async function handleApplyUpdate() {
    applying = true;
    success = null;

    try {
      const next = await applyUpdate();
      settings = settings ? { ...settings, update: next } : settings;
      error = null;
      success = next.message;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to apply the update.';
    } finally {
      applying = false;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event !== 'update.updated' || !settings) {
      return;
    }

    settings = {
      ...settings,
      update: event.data
    };
  }

  onMount(() => {
    void loadSettings();

    const disconnect = connectDaemonStream({
      onEvent: applyStreamEvent,
      onStatusChange: (status) => {
        streamStatus = status;
      },
      onError: (message) => {
        error = message;
      }
    });

    return () => {
      disconnect();
    };
  });
</script>

<svelte:head>
  <title>Nucleus - Settings</title>
</svelte:head>

<div class="space-y-8">
  <section class="space-y-3">
    <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
    <div>
      <h1 class="text-3xl font-semibold text-zinc-50">Settings</h1>
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
        Nucleus runs as a daemon-owned instance. This page shows the active install wiring and the
        source-checkout update state.
      </p>
    </div>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  {#if success}
    <div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">
      {success}
    </div>
  {/if}

  <section class="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
    <Card>
      <CardHeader>
        <CardTitle>Instance</CardTitle>
        <CardDescription>
          These values define which daemon, state tree, and checkout this UI is steering.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        <div class="grid gap-3 sm:grid-cols-2">
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
              <Settings2 class="size-3.5" />
              <span>Name</span>
            </div>
            <div class="mt-2 text-sm font-medium text-zinc-50">{settings?.instance.name ?? 'Nucleus'}</div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
              <Server class="size-3.5" />
              <span>Daemon Bind</span>
            </div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings?.instance.daemon_bind ?? 'Unavailable'}
            </div>
          </div>
        </div>

        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <HardDrive class="size-3.5" />
            <span>State Directory</span>
          </div>
          <div class="mt-2 text-sm text-zinc-100" title={settings?.storage.state_dir}>
            {settings ? compactPath(settings.storage.state_dir) : 'Unavailable'}
          </div>
        </div>

        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <GitBranch class="size-3.5" />
            <span>Repository Root</span>
          </div>
          <div class="mt-2 text-sm text-zinc-100" title={settings?.instance.repo_root}>
            {settings ? compactPath(settings.instance.repo_root) : 'Unavailable'}
          </div>
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>Updates</CardTitle>
        <CardDescription>
          Source-based installs can check the origin remote and fast-forward the checkout from here.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <div class="flex flex-wrap items-center gap-2">
          <Badge variant={update ? updateStateVariant(update.state) : 'secondary'}>
            {update?.state ?? 'idle'}
          </Badge>
          {#if update?.update_available}
            <Badge variant="default">Update available</Badge>
          {/if}
          {#if update?.restart_required}
            <Badge variant="warning">Restart required</Badge>
          {/if}
          {#if update?.dirty_worktree}
            <Badge variant="destructive">Local changes</Badge>
          {/if}
        </div>

        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3 text-sm text-zinc-300">
          {update?.message ?? 'Loading update status...'}
        </div>

        <div class="grid gap-3 sm:grid-cols-2">
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Current</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings?.version ?? '0.0.0'}{#if update?.current_commit_short}
                <span class="text-zinc-500"> - {update.current_commit_short}</span>
              {/if}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Remote</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {#if update?.remote_commit_short}
                {update.remote_name}/{update.branch || 'main'} - {update.remote_commit_short}
              {:else}
                Not checked yet
              {/if}
            </div>
          </div>
        </div>

        {#if update?.restart_required}
          <div class="rounded-md border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
            The checkout is updated. Restart the daemon and web process to load the new code.
          </div>
        {/if}

        {#if update?.install_mode !== 'git'}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3 text-sm text-zinc-400">
            This install is not running from a git checkout, so one-click updates are unavailable.
          </div>
        {/if}

        {#if update?.dirty_worktree}
          <div class="rounded-md border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
            The working tree has local changes. Clean or commit them before applying an update.
          </div>
        {/if}

        <div class="flex flex-wrap items-center gap-3">
          <Button onclick={handleCheckForUpdates} disabled={!canCheck}>
            <RefreshCcw class={checking ? 'size-4 animate-spin' : 'size-4'} />
            {checking ? 'Checking' : 'Check for updates'}
          </Button>

          <Button variant="secondary" onclick={handleApplyUpdate} disabled={!canApply}>
            <Download class={applying ? 'size-4 animate-spin' : 'size-4'} />
            {applying ? 'Updating' : 'Update now'}
          </Button>
        </div>
      </CardContent>
    </Card>
  </section>

  <section class="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
    <Card>
      <CardHeader>
        <CardTitle>Connection</CardTitle>
        <CardDescription>
          These are the daemon-facing URLs for this instance and the current web delivery mode.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <Link class="size-3.5" />
            <span>Local</span>
          </div>
          <div class="mt-2 text-sm text-zinc-100">{settings?.connection.local_url ?? 'Unavailable'}</div>
        </div>

        {#if settings?.connection.hostname_url}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Host</div>
            <div class="mt-2 text-sm text-zinc-100">{settings.connection.hostname_url}</div>
          </div>
        {/if}

        {#if settings?.connection.tailscale_url}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Tailscale</div>
            <div class="mt-2 text-sm text-zinc-100">{settings.connection.tailscale_url}</div>
          </div>
        {/if}

        <div class="grid gap-3 sm:grid-cols-2">
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Web mode</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {formatState(settings?.connection.web_mode ?? 'unknown')}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Auth</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings?.auth.enabled ? 'Bearer token required' : 'Disabled'}
            </div>
          </div>
        </div>

        {#if settings?.connection.web_root}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Web build path</div>
            <div class="mt-2 text-sm text-zinc-100" title={settings.connection.web_root}>
              {compactPath(settings.connection.web_root)}
            </div>
          </div>
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>Access</CardTitle>
        <CardDescription>
          The daemon owns bearer-token auth. The local token is stored outside the repository.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <KeyRound class="size-3.5" />
            <span>Local token path</span>
          </div>
          <div class="mt-2 text-sm text-zinc-100" title={settings?.auth.token_path}>
            {settings ? compactPath(settings.auth.token_path) : 'Unavailable'}
          </div>
        </div>

        <div class="rounded-md border border-amber-400/20 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
          Retrieve the current token with <code>nucleus auth local-token</code>, then use it in the
          web UI or any future client.
        </div>
      </CardContent>
    </Card>
  </section>

  <Card>
    <CardHeader>
      <CardTitle>Update Behavior</CardTitle>
      <CardDescription>
        Nucleus checks for updates in the background and surfaces availability across the app.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-3 text-sm leading-6 text-zinc-400">
      <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <Check class="mt-0.5 size-4 shrink-0 text-lime-300/80" />
        <p>Background checks update the cached daemon state and can raise an in-app toast.</p>
      </div>
      <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <ShieldAlert class="mt-0.5 size-4 shrink-0 text-zinc-500" />
        <p>
          Source-checkout updates fast-forward the repository only. A restart is still required to
          load a new daemon build cleanly.
        </p>
      </div>
    </CardContent>
  </Card>
</div>
