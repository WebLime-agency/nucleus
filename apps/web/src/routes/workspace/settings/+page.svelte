<script lang="ts">
  import { onMount } from 'svelte';
  import {
    Check,
    Download,
    FolderTree,
    GitBranch,
    HardDrive,
    KeyRound,
    Link,
    Power,
    RefreshCcw,
    Save,
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
  import {
    CURRENT_CLIENT_VERSION,
    CURRENT_CLIENT_SURFACE_VERSION,
    evaluateCompatibility
  } from '$lib/nucleus/compatibility';
  import {
    applyUpdate,
    checkForUpdates,
    fetchOverview,
    fetchSettings,
    restartDaemon,
    updateUpdateConfig,
    updateWorkspace
  } from '$lib/nucleus/client';
  import {
    compactPath,
    formatDateTime,
    formatLatestTargetLabel,
    formatState
  } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    RuntimeOverview,
    SettingsSummary,
    WorkspaceSummary
  } from '$lib/nucleus/schemas';

  const releaseChannels = ['stable', 'beta', 'nightly'] as const;

  let settings = $state<SettingsSummary | null>(null);
  let overview = $state<RuntimeOverview | null>(null);
  let loading = $state(true);
  let checking = $state(false);
  let applying = $state(false);
  let restarting = $state(false);
  let savingUpdateConfig = $state(false);
  let savingWorkspace = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');
  let trackedChannelInput = $state('stable');
  let trackedRefInput = $state('');
  let workspaceRoot = $state('');

  let workspace = $derived<WorkspaceSummary | null>(overview?.workspace ?? null);
  let workspaceDirty = $derived(workspace ? workspaceRoot !== workspace.root_path : false);

  let update = $derived(settings?.update ?? null);
  let compatibility = $derived(evaluateCompatibility(settings?.compatibility ?? null));
  let compatibilityWarning = $derived(compatibility.message);
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (checking) return 'Checking';
    if (applying) return 'Updating';
    if (restarting || update?.state === 'restarting') return 'Restarting';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (compatibility.level === 'blocked') return 'Incompatible';
    if (compatibility.level === 'degraded') return 'Degraded';
    if (error) return 'Degraded';
    return 'Live';
  });
  let isDevCheckout = $derived(update?.install_kind === 'dev_checkout');
  let trackedTargetLabel = $derived.by(() => {
    if (!update) {
      return 'Unavailable';
    }

    if (update.tracked_channel) {
      return update.tracked_channel;
    }

    return update.tracked_ref ?? 'Unavailable';
  });
  let latestTargetLabel = $derived.by(() => {
    return formatLatestTargetLabel(update, 'Not checked yet');
  });
  let currentTargetLabel = $derived(isDevCheckout ? 'Current Ref' : 'Current Release ID');
  let updateConfigDirty = $derived.by(() => {
    if (!update) {
      return false;
    }

    if (update.install_kind === 'managed_release') {
      return trackedChannelInput !== (update.tracked_channel ?? 'stable');
    }

    return trackedRefInput.trim() !== (update.tracked_ref ?? '');
  });
  let canSaveUpdateConfig = $derived(
    !!update &&
      !savingUpdateConfig &&
      !checking &&
      !applying &&
      !restarting &&
      update.state !== 'restarting' &&
      updateConfigDirty &&
      (update.install_kind === 'managed_release'
        ? trackedChannelInput.trim().length > 0
        : trackedRefInput.trim().length > 0)
  );
  let canCheck = $derived(
    !!settings &&
      !checking &&
      !applying &&
      !restarting &&
      settings.update.state !== 'restarting'
  );
  let canApply = $derived(
    !!update &&
      update.update_available &&
      !update.dirty_worktree &&
      !update.restart_required &&
      !checking &&
      !applying &&
      !restarting &&
      update.state !== 'restarting'
  );
  let canRestart = $derived(
    !!settings &&
      settings.instance.restart_supported &&
      !checking &&
      !applying &&
      !restarting &&
      settings.update.state !== 'restarting'
  );

  function updateStateVariant(
    value: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (value === 'ready') return 'default';
    if (value === 'checking' || value === 'applying' || value === 'restarting') return 'warning';
    if (value === 'unsupported' || value === 'idle') return 'secondary';
    return 'destructive';
  }

  function restartModeLabel(value: string) {
    if (value === 'systemd') return 'Systemd service';
    if (value === 'self-reexec') return 'Managed process';
    return 'Manual only';
  }

  function installKindLabel(value: string) {
    if (value === 'dev_checkout') return 'Dev checkout';
    if (value === 'managed_release') return 'Managed release';
    return formatState(value);
  }

  function applySettings(next: SettingsSummary) {
    settings = next;
    trackedChannelInput = next.update.tracked_channel ?? 'stable';
    trackedRefInput = next.update.tracked_ref ?? '';

    if (next.update.state !== 'restarting') {
      restarting = false;
    }
  }

  async function loadSettings() {
    loading = settings === null;

    try {
      applySettings(await fetchSettings());
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load settings.';
    } finally {
      loading = false;
    }
  }

  function applyOverview(next: RuntimeOverview, force = false) {
    overview = next;

    if (force || !workspaceDirty) {
      workspaceRoot = next.workspace.root_path;
    }
  }

  async function loadOverview() {
    try {
      applyOverview(await fetchOverview(), true);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load workspace state.';
    }
  }

  async function handleSaveWorkspace() {
    if (!workspaceRoot.trim()) {
      error = 'Workspace root is required.';
      return;
    }

    savingWorkspace = true;
    success = null;

    try {
      const nextWorkspace = await updateWorkspace({ root_path: workspaceRoot });
      if (overview) {
        applyOverview({ ...overview, workspace: nextWorkspace }, true);
      }
      error = null;
      success = 'Workspace root updated.';
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the workspace root.';
    } finally {
      savingWorkspace = false;
    }
  }

  async function handleCheckForUpdates() {
    checking = true;
    success = null;

    try {
      const next = await checkForUpdates();
      if (settings) {
        applySettings({ ...settings, update: next });
      }
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
      if (settings) {
        applySettings({ ...settings, update: next });
      }
      restarting = next.state === 'restarting';
      error = null;
      success = next.message;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to apply the update.';
    } finally {
      applying = false;
    }
  }

  async function handleRestartDaemon() {
    restarting = true;
    success = null;

    try {
      const next = await restartDaemon();
      if (settings) {
        applySettings({ ...settings, update: next });
      }
      error = null;
      success = next.message;
    } catch (cause) {
      restarting = false;
      error = cause instanceof Error ? cause.message : 'Failed to restart the daemon.';
    }
  }

  async function handleSaveUpdateConfig() {
    if (!update) {
      return;
    }

    savingUpdateConfig = true;
    success = null;

    try {
      const next =
        update.install_kind === 'managed_release'
          ? await updateUpdateConfig({ tracked_channel: trackedChannelInput })
          : await updateUpdateConfig({ tracked_ref: trackedRefInput.trim() });
      if (settings) {
        applySettings({ ...settings, update: next });
      }
      error = null;
      success = next.message;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the tracked target.';
    } finally {
      savingUpdateConfig = false;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event === 'overview.updated') {
      applyOverview(event.data);
      return;
    }

    if (event.event === 'update.updated' && settings) {
      applySettings({
        ...settings,
        update: event.data
      });
    }
  }

  onMount(() => {
    void loadSettings();
    void loadOverview();

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
    <Badge variant={error || compatibility.level === 'blocked' ? 'destructive' : 'default'}>{statusLabel}</Badge>
    <div>
      <h1 class="text-3xl font-semibold text-zinc-50">Settings</h1>
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
        Nucleus runs as a daemon-owned instance. This page shows the active install wiring, the
        tracked update target, and the daemon-owned update history.
      </p>
    </div>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  {#if compatibilityWarning}
    <div
      class={compatibility.level === 'blocked'
        ? 'rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-100'
        : 'rounded-lg border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100'}
    >
      {compatibilityWarning}
    </div>
  {/if}

  {#if success}
    <div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">
      {success}
    </div>
  {/if}

  <Card>
    <CardHeader>
      <CardTitle>Workspace</CardTitle>
      <CardDescription>
        The workspace root is where the daemon discovers projects. Sessions still pick which
        projects to attach when work starts.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <label class="block space-y-1">
        <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Root Path</span>
        <input
          class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
          bind:value={workspaceRoot}
          placeholder="/home/eba/dev-projects"
        />
      </label>

      <Button
        onclick={handleSaveWorkspace}
        disabled={savingWorkspace || !workspaceDirty || !workspaceRoot.trim()}
      >
        <Save class={savingWorkspace ? 'size-4 animate-spin' : 'size-4'} />
        {savingWorkspace ? 'Saving' : 'Save Workspace Root'}
      </Button>

      <div class="space-y-3 pt-2">
        <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">
          Discovered Projects
        </div>
        {#if !workspace || workspace.projects.length === 0}
          <div class="rounded-md border border-dashed border-zinc-800 px-4 py-6 text-sm text-zinc-500">
            No projects discovered yet. Save a valid root and the daemon will populate them.
          </div>
        {:else}
          {#each workspace.projects as project}
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
              <div class="flex flex-col gap-2 lg:flex-row lg:items-start lg:justify-between">
                <div class="space-y-1">
                  <div class="flex items-center gap-2">
                    <FolderTree class="size-4 text-zinc-500" />
                    <div class="font-medium text-zinc-100">{project.title}</div>
                    <Badge variant="secondary">Discovered</Badge>
                  </div>
                  <div class="text-sm text-zinc-400">{project.relative_path}</div>
                  <div class="text-xs text-zinc-500">{compactPath(project.absolute_path)}</div>
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </CardContent>
  </Card>

  <section class="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
    <Card>
      <CardHeader>
        <CardTitle>Instance</CardTitle>
        <CardDescription>
          These values define which daemon, state tree, and install shape this UI is steering.
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
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
              <Power class="size-3.5" />
              <span>Restart Control</span>
            </div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings ? restartModeLabel(settings.instance.restart_mode) : 'Unavailable'}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
              <GitBranch class="size-3.5" />
              <span>Install Kind</span>
            </div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings ? installKindLabel(settings.instance.install_kind) : 'Unavailable'}
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

        {#if settings?.instance.repo_root}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
              <GitBranch class="size-3.5" />
              <span>Repository Root</span>
            </div>
            <div class="mt-2 text-sm text-zinc-100" title={settings.instance.repo_root}>
              {compactPath(settings.instance.repo_root)}
            </div>
          </div>
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>Updates</CardTitle>
        <CardDescription>
          The daemon owns the tracked target, latest successful check, latest attempted check, and
          restart requirement for this install.
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
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Current Version</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {settings?.version ?? '0.0.0'}
            </div>
            {#if update?.current_commit_short}
              <div class="mt-1 text-xs text-zinc-500">{update.current_commit_short}</div>
            {/if}
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Tracked Target</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {trackedTargetLabel}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Latest Known Target</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">{latestTargetLabel}</div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">{currentTargetLabel}</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {update?.current_ref ?? (isDevCheckout ? 'Detached or unknown' : 'Not applicable')}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Last Successful Check</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {update?.last_successful_check_at
                ? formatDateTime(update.last_successful_check_at)
                : 'Never'}
            </div>
          </div>
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
            <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Last Attempted Check</div>
            <div class="mt-2 text-sm font-medium text-zinc-50">
              {update?.last_attempted_check_at
                ? formatDateTime(update.last_attempted_check_at)
                : 'Never'}
            </div>
            <div class="mt-1 text-xs text-zinc-500">
              {update?.last_attempt_result ? formatState(update.last_attempt_result) : 'No attempts yet'}
            </div>
          </div>
        </div>

        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4">
          <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">
            {isDevCheckout ? 'Tracked Git Ref' : 'Tracked Release Channel'}
          </div>

          {#if update?.install_kind === 'managed_release'}
            <div class="mt-3 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
              <select
                class="h-11 rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                bind:value={trackedChannelInput}
                aria-label="Tracked release channel"
              >
                {#each releaseChannels as channel}
                  <option value={channel}>{channel}</option>
                {/each}
              </select>
              <Button onclick={handleSaveUpdateConfig} disabled={!canSaveUpdateConfig}>
                {savingUpdateConfig ? 'Saving' : 'Save target'}
              </Button>
            </div>
            <div class="mt-3 text-xs leading-5 text-zinc-500">
              Managed installs follow release channels, not git branches. The daemon stores the
              tracked channel separately from the currently running release and reuses it across
              reconnects and restarts.
            </div>
          {:else}
            <div class="mt-3 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
              <input
                class="h-11 rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                bind:value={trackedRefInput}
                placeholder="main"
                spellcheck="false"
                autocapitalize="off"
                aria-label="Tracked git ref"
              />
              <Button onclick={handleSaveUpdateConfig} disabled={!canSaveUpdateConfig}>
                {savingUpdateConfig ? 'Saving' : 'Save target'}
              </Button>
            </div>
            <div class="mt-3 text-xs leading-5 text-zinc-500">
              Contributor installs can track an explicit ref such as <code>main</code>. The daemon
              keeps this target separate from the live checkout so mismatch states stay visible.
            </div>
          {/if}
        </div>

        {#if update?.restart_required}
          <div class="rounded-md border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
            The install payload is newer than the running daemon. Restart the daemon after
            resolving the issue.
          </div>
        {/if}

        {#if update?.state === 'restarting' || restarting}
          <div class="rounded-md border border-sky-400/30 bg-sky-400/10 px-4 py-3 text-sm text-sky-100">
            Nucleus is restarting now. This page should reconnect automatically.
          </div>
        {/if}

        {#if update?.dirty_worktree}
          <div class="rounded-md border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
            The working tree has local changes. Clean or commit them before applying an update.
          </div>
        {/if}

        {#if update?.latest_error}
          <div class="rounded-md border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
            <div class="font-medium text-red-100">Latest error</div>
            <div class="mt-1">{update.latest_error}</div>
            {#if update.latest_error_at}
              <div class="mt-1 text-xs text-red-100/80">{formatDateTime(update.latest_error_at)}</div>
            {/if}
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

          <Button variant="secondary" onclick={handleRestartDaemon} disabled={!canRestart}>
            <Power class={restarting || update?.state === 'restarting' ? 'size-4 animate-pulse' : 'size-4'} />
            {restarting || update?.state === 'restarting' ? 'Restarting' : 'Restart daemon'}
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
      <CardTitle>Compatibility</CardTitle>
      <CardDescription>
        Clients should rely on explicit daemon compatibility metadata instead of inferring support
        from transport or decode failures.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-3">
      <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <ShieldAlert class="size-3.5" />
            <span>Client Version</span>
          </div>
          <div class="mt-2 text-sm font-medium text-zinc-50">{CURRENT_CLIENT_VERSION}</div>
        </div>
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
            <ShieldAlert class="size-3.5" />
            <span>Client Surface</span>
          </div>
          <div class="mt-2 text-sm font-medium text-zinc-50">{CURRENT_CLIENT_SURFACE_VERSION}</div>
        </div>
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Daemon Surface</div>
          <div class="mt-2 text-sm font-medium text-zinc-50">
            {settings?.compatibility.surface_version ?? 'Unavailable'}
          </div>
        </div>
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Minimum Client</div>
          <div class="mt-2 text-sm font-medium text-zinc-50">
            {settings?.compatibility.minimum_client_version ?? 'Not set'}
          </div>
        </div>
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
          <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Minimum Server</div>
          <div class="mt-2 text-sm font-medium text-zinc-50">
            {settings?.compatibility.minimum_server_version ?? 'Not set'}
          </div>
        </div>
      </div>

      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Capability Flags</div>
        {#if settings?.compatibility.capability_flags.length}
          <div class="mt-3 flex flex-wrap gap-2">
            {#each settings.compatibility.capability_flags as capability}
              <Badge variant="secondary">{capability}</Badge>
            {/each}
          </div>
        {:else}
          <div class="mt-2 text-sm text-zinc-500">No capability flags were published.</div>
        {/if}
      </div>
    </CardContent>
  </Card>

  <Card>
    <CardHeader>
      <CardTitle>Update Behavior</CardTitle>
      <CardDescription>
        Nucleus keeps update truth in the daemon and serves the embedded web client that matches
        the running daemon release.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-3 text-sm leading-6 text-zinc-400">
      <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <Check class="mt-0.5 size-4 shrink-0 text-lime-300/80" />
        <p>
          Background checks update daemon-owned state and only raise an in-app toast when the latest
          successful check found a newer target.
        </p>
      </div>
      <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <ShieldAlert class="mt-0.5 size-4 shrink-0 text-zinc-500" />
        <p>
          Dev checkouts may still use git-based updates. Managed releases now resolve channel
          artifacts, verify them, swap them into place, and restart onto the matching embedded web
          build instead of pulling branches directly.
        </p>
      </div>
    </CardContent>
  </Card>
</div>
