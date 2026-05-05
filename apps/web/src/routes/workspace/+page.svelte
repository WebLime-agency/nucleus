<script lang="ts">
  import { onMount } from 'svelte';
  import { Bot, FolderTree, Router, Save } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { fetchOverview, updateWorkspace } from '$lib/nucleus/client';
  import { compactPath, formatCount, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, RuntimeOverview, WorkspaceSummary } from '$lib/nucleus/schemas';
  import {
    buildWorkspaceTargetOptions,
    describeWorkspaceTarget
  } from '$lib/nucleus/targets';

  let overview = $state<RuntimeOverview | null>(null);
  let workspaceRoot = $state('');
  let mainTarget = $state('');
  let utilityTarget = $state('');
  let loading = $state(true);
  let refreshing = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let workspace = $derived(overview?.workspace ?? null);
  let routerProfiles = $derived(overview?.router_profiles ?? []);
  let runtimes = $derived(overview?.runtimes ?? []);
  let targetOptions = $derived(buildWorkspaceTargetOptions(routerProfiles, runtimes));
  let selectedMainTarget = $derived(
    describeWorkspaceTarget(mainTarget, routerProfiles, runtimes)
  );
  let selectedUtilityTarget = $derived(
    describeWorkspaceTarget(utilityTarget, routerProfiles, runtimes)
  );
  let workspaceSettingsDirty = $derived(
    workspace
      ? workspaceRoot !== workspace.root_path ||
          mainTarget !== workspace.main_target ||
          utilityTarget !== workspace.utility_target
      : false
  );
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (error) return 'Degraded';
    return 'Live';
  });

  function syncWorkspaceFields(nextWorkspace: WorkspaceSummary, force = false) {
    if (!force && workspaceSettingsDirty) {
      return;
    }

    workspaceRoot = nextWorkspace.root_path;
    mainTarget = nextWorkspace.main_target;
    utilityTarget = nextWorkspace.utility_target;
  }

  async function loadAll(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      const nextOverview = await fetchOverview();
      overview = nextOverview;
      syncWorkspaceFields(nextOverview.workspace, true);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read workspace state.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function handleSaveWorkspace() {
    if (!workspaceRoot.trim()) {
      error = 'Workspace root is required.';
      return;
    }

    if (!mainTarget) {
      error = 'Workspace main model is required.';
      return;
    }

    if (!utilityTarget) {
      error = 'Workspace utility model is required.';
      return;
    }

    saving = true;
    success = null;

    try {
      const nextWorkspace = await updateWorkspace({
        root_path: workspaceRoot,
        main_target: mainTarget,
        utility_target: utilityTarget
      });

      overview = overview
        ? { ...overview, workspace: nextWorkspace }
        : null;
      syncWorkspaceFields(nextWorkspace, true);
      success = 'Workspace settings updated.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update workspace settings.';
    } finally {
      saving = false;
    }
  }

  function routeStateVariant(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'ready') return 'default';
    if (state === 'degraded') return 'warning';
    if (state === 'disabled') return 'secondary';
    return 'destructive';
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event !== 'overview.updated') {
      return;
    }

    overview = event.data;
    syncWorkspaceFields(event.data.workspace);
    loading = false;
    refreshing = false;
    error = null;
  }

  onMount(() => {
    void loadAll();

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
  <title>Nucleus - Workspace</title>
</svelte:head>

<div class="space-y-8">
  <section class="space-y-3">
    <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
    <div>
      <h1 class="text-3xl font-semibold text-zinc-50">Workspace</h1>
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
        The daemon owns workspace discovery and target definitions. This page only edits the root
        path plus the workspace-wide main and utility model targets.
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

  <section class="grid gap-4 xl:grid-cols-[0.88fr_1.12fr]">
    <Card>
      <CardHeader>
        <CardTitle>Workspace Settings</CardTitle>
        <CardDescription>
          Saving here updates daemon-owned settings. Project discovery refreshes from the root automatically.
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

        <label class="block space-y-1">
          <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Main Model</span>
          <select
            class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
            bind:value={mainTarget}
            disabled={targetOptions.length === 0}
          >
            {#if targetOptions.length === 0}
              <option value="">No targets available</option>
            {:else}
              {#each targetOptions as option}
                <option value={option.value}>{option.label}</option>
              {/each}
            {/if}
          </select>
          <div class="text-xs text-zinc-500">{selectedMainTarget.helper}</div>
        </label>

        <label class="block space-y-1">
          <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Utility Model</span>
          <select
            class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
            bind:value={utilityTarget}
            disabled={targetOptions.length === 0}
          >
            {#if targetOptions.length === 0}
              <option value="">No targets available</option>
            {:else}
              {#each targetOptions as option}
                <option value={option.value}>{option.label}</option>
              {/each}
            {/if}
          </select>
          <div class="text-xs text-zinc-500">{selectedUtilityTarget.helper}</div>
        </label>

          <div class="grid gap-3 sm:grid-cols-3">
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
              <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Projects</div>
              <div class="mt-2 text-2xl font-semibold text-zinc-50">
                {formatCount(workspace?.projects.length ?? 0)}
              </div>
            </div>
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
              <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Router Profiles</div>
              <div class="mt-2 text-2xl font-semibold text-zinc-50">
                {formatCount(routerProfiles.length)}
              </div>
            </div>
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
              <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Ready Runtimes</div>
              <div class="mt-2 text-2xl font-semibold text-zinc-50">
              {formatCount(runtimes.filter((runtime) => runtime.state === 'ready').length)}
            </div>
          </div>
        </div>

        <Button
          onclick={handleSaveWorkspace}
          disabled={saving || !workspaceRoot.trim() || !mainTarget || !utilityTarget || !workspaceSettingsDirty}
        >
          <Save class={saving ? 'size-4 animate-spin' : 'size-4'} />
          {saving ? 'Saving' : 'Save Workspace'}
        </Button>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>Discovered Projects</CardTitle>
        <CardDescription>
          These directories are discovered from the workspace root. Session attachment is managed from Sessions.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        {#if !workspace || workspace.projects.length === 0}
          <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
            No projects discovered yet. Save a valid root and the daemon will populate them.
          </div>
        {:else}
          {#each workspace.projects as project}
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4">
              <div class="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
                <div class="space-y-1">
                  <div class="flex items-center gap-2">
                    <FolderTree class="size-4 text-zinc-500" />
                    <div class="font-medium text-zinc-100">{project.title}</div>
                    <Badge variant="secondary">Discovered</Badge>
                  </div>
                  <div class="text-sm text-zinc-400">{project.relative_path}</div>
                  <div class="text-xs text-zinc-500">{compactPath(project.absolute_path)}</div>
                </div>

                <div class="rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-2 text-xs text-zinc-500">
                  Attach from a session when needed
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </CardContent>
    </Card>
  </section>

  <Card>
    <CardHeader>
      <CardTitle>Router Profiles</CardTitle>
      <CardDescription>Ordered provider targets stored in the daemon routing layer.</CardDescription>
    </CardHeader>
    <CardContent class="space-y-3">
      {#if routerProfiles.length === 0}
        <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
          No router profiles are configured yet.
        </div>
      {:else}
        {#each routerProfiles as profile}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4">
            <div class="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
              <div class="space-y-1">
                <div class="flex items-center gap-2">
                  <Router class="size-4 text-zinc-500" />
                  <div class="font-medium text-zinc-100">{profile.title}</div>
                  <Badge variant={routeStateVariant(profile.state)}>{formatState(profile.state)}</Badge>
                </div>
                <div class="text-sm text-zinc-400">{profile.summary}</div>
              </div>

              <div class="text-xs text-zinc-500">
                {profile.enabled ? 'Enabled' : 'Disabled'}
              </div>
            </div>

            <div class="mt-4 grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {#each profile.targets as target, index}
                <div class="rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-3">
                  <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
                    <Bot class="size-3.5" />
                    Target {index + 1}
                  </div>
                  <div class="mt-2 text-sm font-medium text-zinc-100">
                    {formatState(target.provider)}
                  </div>
                  <div class="mt-1 text-xs text-zinc-500">{target.model || 'Provider default model'}</div>
                </div>
              {/each}
            </div>
          </div>
        {/each}
      {/if}
    </CardContent>
  </Card>
</div>
