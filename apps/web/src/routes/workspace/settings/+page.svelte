<script lang="ts">
  import { onMount } from 'svelte';
  import {
    Check,
    Clock3,
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
  import {
    WorkspaceAccessCard,
    WorkspaceCompatibilityCard,
    WorkspaceConnectionCard,
    WorkspaceInfoTile,
    WorkspaceNoteGrid,
    WorkspacePageHeader,
    WorkspaceStoragePathCard,
    WorkspaceUpdateBehaviorCard,
    WorkspaceUpdateControlsCard,
    WorkspaceUpdateStatusGrid,
    WorkspaceUpdateTargetCard
  } from '$lib/components/app/workspace';
  import { Button } from '$lib/components/ui/button';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
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
  let runBudgetMaxSteps = $state(80);
  let runBudgetMaxActions = $state(160);
  let runBudgetMaxWallClockHours = $state(2);

  let workspace = $derived<WorkspaceSummary | null>(overview?.workspace ?? null);
  let workspaceDirty = $derived(workspace ? workspaceRoot !== workspace.root_path : false);
  let runBudgetDirty = $derived.by(() => {
    if (!workspace) {
      return false;
    }

    return (
      runBudgetMaxSteps !== workspace.run_budget.max_steps ||
      runBudgetMaxActions !== workspace.run_budget.max_tool_calls ||
      Math.round(runBudgetMaxWallClockHours * 3600) !== workspace.run_budget.max_wall_clock_secs
    );
  });

  let update = $derived(settings?.update ?? null);
  let compatibility = $derived(evaluateCompatibility(settings?.compatibility ?? null));
  let statusLabel = $derived.by(() => {
    if (error) return 'Error';
    if (restarting || settings?.update?.state === 'restarting') return 'Restarting';
    if (checking) return 'Checking for updates';
    if (applying) return 'Applying update';
    if (streamStatus === 'connected') return 'Connected';
    if (streamStatus === 'connecting') return 'Connecting';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    return 'Offline';
  });
  let compatibilityWarning = $derived(compatibility.message);
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
    if (force || !runBudgetDirty) {
      applyWorkspaceBudgetInputs(next.workspace);
    }
  }

  function applyWorkspaceBudgetInputs(next: WorkspaceSummary) {
    runBudgetMaxSteps = next.run_budget.max_steps;
    runBudgetMaxActions = next.run_budget.max_tool_calls;
    runBudgetMaxWallClockHours = Math.max(
      1,
      Math.round((next.run_budget.max_wall_clock_secs / 3600) * 10) / 10
    );
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

  async function handleSaveRunBudget() {
    if (runBudgetMaxSteps < 1 || runBudgetMaxActions < 1 || runBudgetMaxWallClockHours <= 0) {
      error = 'Run budget values must be greater than zero.';
      return;
    }

    savingWorkspace = true;
    success = null;

    try {
      const nextWorkspace = await updateWorkspace({
        run_budget: {
          mode: 'standard',
          max_steps: runBudgetMaxSteps,
          max_tool_calls: runBudgetMaxActions,
          max_wall_clock_secs: Math.round(runBudgetMaxWallClockHours * 3600)
        }
      });
      if (overview) {
        applyOverview({ ...overview, workspace: nextWorkspace }, true);
      }
      error = null;
      success = 'Default run budget updated.';
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the default run budget.';
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
      error = cause instanceof Error ? cause.message : 'Failed to restart Nucleus.';
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
        Nucleus runs as a managed local instance. This page shows the active install wiring, the
        tracked update target, and Nucleus-owned update history.
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
        The workspace root is where Nucleus discovers projects. Sessions still pick which
        projects to attach when work starts.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="block space-y-1">
        <Label for="workspace-root">Root Path</Label>
        <Input
          id="workspace-root"
          bind:value={workspaceRoot}
          placeholder="/home/eba/dev-projects"
        />
      </div>

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
            No projects discovered yet. Save a valid root and Nucleus will populate them.
          </div>
        {:else}
          {#each workspace.projects as project}
            <div class="min-w-0 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
              <div class="flex flex-col gap-2 lg:flex-row lg:items-start lg:justify-between">
                <div class="min-w-0 space-y-1">
                  <div class="flex min-w-0 items-center gap-2">
                    <FolderTree class="size-4 text-zinc-500" />
                    <div class="min-w-0 truncate font-medium text-zinc-100">{project.title}</div>
                    <Badge variant="secondary">Discovered</Badge>
                  </div>
                  <div class="truncate text-sm text-zinc-400">{project.relative_path}</div>
                  <div class="truncate text-xs text-zinc-500">{compactPath(project.absolute_path)}</div>
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </CardContent>
  </Card>

  <Card>
    <CardHeader>
      <CardTitle>Run Budget</CardTitle>
      <CardDescription>
        Default Utility Worker limits for new session turns. Sessions can inherit these defaults
        or choose a different run budget from the composer.
      </CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid gap-3 md:grid-cols-3">
        <div class="block space-y-1">
          <Label for="run-budget-steps">Conversation steps</Label>
          <Input
            id="run-budget-steps"
            type="number"
            min="1"
            max="1000"
            bind:value={runBudgetMaxSteps}
            aria-label="Default maximum steps"
          />
          <span class="block text-xs leading-5 text-zinc-500">How many reasoning or work steps a Utility Worker can take.</span>
        </div>

        <div class="block space-y-1">
          <Label for="run-budget-actions">Actions</Label>
          <Input
            id="run-budget-actions"
            type="number"
            min="1"
            max="2000"
            bind:value={runBudgetMaxActions}
            aria-label="Default maximum actions"
          />
          <span class="block text-xs leading-5 text-zinc-500">Commands, file edits, searches, and other concrete actions.</span>
        </div>

        <div class="block space-y-1">
          <Label for="run-budget-hours">Time limit</Label>
          <Input
            id="run-budget-hours"
            type="number"
            min="0.1"
            max="24"
            step="0.5"
            bind:value={runBudgetMaxWallClockHours}
            aria-label="Default maximum run time in hours"
          />
          <span class="block text-xs leading-5 text-zinc-500">Maximum elapsed time before the turn stops.</span>
        </div>
      </div>

      <div class="flex flex-wrap items-center gap-3">
        <Button
          onclick={handleSaveRunBudget}
          disabled={
            savingWorkspace ||
            !runBudgetDirty ||
            runBudgetMaxSteps < 1 ||
            runBudgetMaxActions < 1 ||
            runBudgetMaxWallClockHours <= 0
          }
        >
          <Clock3 class={savingWorkspace ? 'size-4 animate-spin' : 'size-4'} />
          {savingWorkspace ? 'Saving' : 'Save Run Budget'}
        </Button>
        <div class="text-xs leading-5 text-zinc-500">
          Use <span class="text-zinc-300">Unbounded</span> at the session level only for trusted
          local work where long-running autonomy is expected.
        </div>
      </div>

      <WorkspaceNoteGrid
        items={[
          { title: 'Focused', detail: 'everyday chat, small fixes, and quick checks. 80 steps, 160 actions, 2 hours.' },
          { title: 'Extended', detail: 'longer coding or research tasks. 200 steps, 400 actions, 4 hours.' },
          { title: 'Marathon', detail: 'several hours of supervised local work. 600 steps, 1200 actions, 8 hours.' },
          { title: 'Unbounded', detail: 'trusted work that should keep going until stopped. No step, action, or time cap.' }
        ]}
      />
    </CardContent>
  </Card>

  <section class="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
    <Card>
      <CardHeader>
        <CardTitle>Instance</CardTitle>
        <CardDescription>
          These values define which Nucleus instance, state tree, and install shape this UI is steering.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        <div class="grid gap-3 sm:grid-cols-2">
          <WorkspaceInfoTile label="Name" value={settings?.instance.name ?? 'Nucleus'} icon={Settings2} />
          <WorkspaceInfoTile
            label="Server Bind"
            value={settings?.instance.daemon_bind ?? 'Unavailable'}
            icon={Server}
          />
          <WorkspaceInfoTile
            label="Restart Control"
            value={settings ? restartModeLabel(settings.instance.restart_mode) : 'Unavailable'}
            icon={Power}
          />
          <WorkspaceInfoTile
            label="Install Kind"
            value={settings ? installKindLabel(settings.instance.install_kind) : 'Unavailable'}
            icon={GitBranch}
          />
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
          Nucleus owns the tracked target, latest successful check, latest attempted check, and
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
        </div>

        <WorkspaceUpdateStatusGrid
          latestTargetLabel={trackedTargetLabel}
          currentVersionLabel={latestTargetLabel}
          updateStateLabel={currentTargetLabel}
          lastSuccessfulCheckAt={update?.last_successful_check_at ?? null}
          lastAttemptedCheckAt={update?.last_attempted_check_at ?? null}
          lastAttemptResult={update?.last_attempt_result ?? null}
        />

        <WorkspaceUpdateTargetCard
          managedRelease={update?.install_kind === 'managed_release'}
          {releaseChannels}
          {trackedChannelInput}
          {trackedRefInput}
          {canSaveUpdateConfig}
          {savingUpdateConfig}
          onTrackedChannelInput={(value) => {
            trackedChannelInput = value;
          }}
          onTrackedRefInput={(value) => {
            trackedRefInput = value;
          }}
          onSave={() => {
            void handleSaveUpdateConfig();
          }}
        />

        <WorkspaceUpdateControlsCard
          restartRequired={update?.restart_required ?? false}
          restarting={update?.state === 'restarting' || restarting}
          dirtyWorktree={update?.dirty_worktree ?? false}
          latestError={update?.latest_error ?? null}
          latestErrorAt={update?.latest_error_at ?? null}
          {checking}
          {applying}
          {canCheck}
          {canApply}
          {canRestart}
          onCheck={() => {
            void handleCheckForUpdates();
          }}
          onApply={() => {
            void handleApplyUpdate();
          }}
          onRestart={() => {
            void handleRestartDaemon();
          }}
        />
      </CardContent>
    </Card>
  </section>

  <section class="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
    <WorkspaceConnectionCard
      localUrl={settings?.connection.local_url ?? 'Unavailable'}
      hostnameUrl={settings?.connection.hostname_url}
      tailscaleUrl={settings?.connection.tailscale_url}
      webMode={settings?.connection.web_mode ?? 'unknown'}
      authEnabled={settings?.auth.enabled ?? false}
      webRoot={settings?.connection.web_root}
      security={settings?.security ?? null}
    />

    <WorkspaceAccessCard tokenPath={settings?.auth.token_path} />
  </section>

  <WorkspaceCompatibilityCard
    clientVersion={CURRENT_CLIENT_VERSION}
    clientSurfaceVersion={CURRENT_CLIENT_SURFACE_VERSION}
    serverSurfaceVersion={settings?.compatibility.surface_version ?? 'Unavailable'}
    minimumClientVersion={settings?.compatibility.minimum_client_version ?? 'Not set'}
    minimumServerVersion={settings?.compatibility.minimum_server_version ?? 'Not set'}
    capabilityFlags={settings?.compatibility.capability_flags ?? []}
  />

  <WorkspaceUpdateBehaviorCard />
</div>
