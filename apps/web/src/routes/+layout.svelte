<script lang="ts">
  import { browser } from '$app/environment';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import {
    FolderKanban,
    FolderTree,
    Gauge,
    MessageSquare,
    Menu,
    MessageSquarePlus,
    PencilLine,
    Workflow
  } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import AppSidebar from '$lib/components/app/sidebar/app-sidebar.svelte';
  import { Button } from '$lib/components/ui/button';
  import { Dialog, DialogContent, DialogDescription, DialogTitle } from '$lib/components/ui/dialog';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
  import { evaluateCompatibility } from '$lib/nucleus/compatibility';
  import {
    clearAccessToken,
    isAuthError,
    readAccessToken,
    writeAccessToken
  } from '$lib/nucleus/auth';
  import { createSession, fetchOverview, fetchSettings } from '$lib/nucleus/client';
  import {
    formatCount,
    formatLatestTargetLabel,
    formatState
  } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    CompatibilitySummary,
    DaemonEvent,
    RuntimeOverview,
    SessionSummary,
    SettingsSummary
  } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';
  import '../app.css';

  let { children } = $props();

  type AttachmentMode = 'new_worktree' | 'project_root' | 'scratch';

  const navigation = [
    { href: '/', label: 'Chat', icon: MessageSquare },
    { href: '/dashboard', label: 'Dashboard', icon: Gauge },
    { href: '/automations', label: 'Automations', icon: Workflow },
    { href: '/workspace', label: 'Workspace', icon: FolderTree }
  ];

  const ATTACHMENT_MODE_OPTIONS: {
    value: AttachmentMode;
    label: string;
    description: string;
    icon: typeof import('lucide-svelte').Icon;
  }[] = [
    {
      value: 'new_worktree',
      label: 'New worktree session',
      description: 'Create a separate checkout for this session.',
      icon: FolderKanban
    },
    {
      value: 'project_root',
      label: 'New project-root session',
      description: 'Work directly in the selected project checkout.',
      icon: PencilLine
    },
    {
      value: 'scratch',
      label: 'New scratch session',
      description: 'Start without an attached project.',
      icon: MessageSquarePlus
    }
  ];

  let overview = $state<RuntimeOverview | null>(null);
  let settings = $state<SettingsSummary | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let creating = $state(false);
  let error = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');
  let createDialogOpen = $state(false);
  let createProjectId = $state('');
  let createAttachmentMode = $state<AttachmentMode>('new_worktree');
  let createTitle = $state('');
  let sidebarOpen = $state(false);
  let updateToastVisible = $state(false);
  let dismissedUpdateTarget = $state('');
  let authPromptVisible = $state(false);
  let authTokenInput = $state('');
  let authSubmitting = $state(false);
  let authMessage = $state<string | null>(null);
  let daemonCompatibility = $state<CompatibilitySummary | null>(null);

  let pathname = $derived(page.url.pathname);
  let workspace = $derived(overview?.workspace ?? null);
  let discoveredProjects = $derived(workspace?.projects ?? []);
  let defaultProfileTitle = $derived(
    workspace?.profiles.find((profile) => profile.id === workspace.default_profile_id)?.title ??
      'Default'
  );
  let sessions = $derived(overview?.sessions ?? []);
  let selectedCreateProject = $derived(
    discoveredProjects.find((project) => project.id === createProjectId) ?? null
  );
  let createRequiresProject = $derived(createAttachmentMode !== 'scratch');
  let createCanSubmit = $derived(!createRequiresProject || Boolean(createProjectId));
  let updateStatus = $derived(settings?.update ?? null);
  let hasUpdateAvailable = $derived(updateStatus?.update_available ?? false);
  let restartRequired = $derived(updateStatus?.restart_required ?? false);
  let updateTrackLabel = $derived.by(() => {
    if (!updateStatus) {
      return '';
    }

    if (updateStatus.tracked_channel) {
      return updateStatus.tracked_channel;
    }

    return updateStatus.tracked_ref ?? '';
  });
  let updateTargetLabel = $derived.by(() => {
    return formatLatestTargetLabel(updateStatus, 'A newer build');
  });
  function isNavActive(href: string, currentPath: string) {
    if (href === '/') {
      return currentPath === '/';
    }
    return currentPath === href || currentPath.startsWith(`${href}/`);
  }
  let activeNavItem = $derived(
    navigation.find((item) => isNavActive(item.href, pathname)) ?? navigation[0]
  );
  let requestedSessionId = $derived.by(() =>
    browser ? page.url.searchParams.get('session') ?? '' : ''
  );
  let usesFullHeightContent = $derived(pathname === '/' || pathname.startsWith('/workspace'));
  let activeSidebarSessionId = $derived(requestedSessionId || sessions[0]?.id || '');
  let compatibility = $derived(
    evaluateCompatibility(daemonCompatibility ?? settings?.compatibility ?? null)
  );
  let compatibilityWarning = $derived(compatibility.message);
  let compatibilityBlocked = $derived(compatibility.level === 'blocked');
  let createSessionTitle = $derived.by(() => {
    if (createTitle.trim()) {
      return createTitle.trim();
    }

    if (createAttachmentMode === 'scratch') {
      return `New ${defaultProfileTitle} session`;
    }

    if (!selectedCreateProject) {
      return 'New session';
    }

    return `New ${defaultProfileTitle} session from ${selectedCreateProject.title}`;
  });
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (authPromptVisible) return 'Auth required';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (compatibilityBlocked) return 'Incompatible';
    if (compatibility.level === 'degraded') return 'Degraded';
    if (error) return 'Degraded';
    return 'Live';
  });

  function syncCreateDefaults() {
    if (!discoveredProjects.some((project) => project.id === createProjectId)) {
      createProjectId = discoveredProjects[0]?.id ?? '';
    }

    if (discoveredProjects.length === 0 && createAttachmentMode !== 'scratch') {
      createAttachmentMode = 'scratch';
    }
  }

  function openCreateSessionDialog() {
    syncCreateDefaults();
    error = null;
    createDialogOpen = true;
  }

  function badgeVariantForSession(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'active') return 'default';
    if (state === 'running') return 'warning';
    if (state === 'archived') return 'secondary';
    return 'destructive';
  }

  function sessionContextLabel() {
    return 'Workspace scratch';
  }

  function projectLabel(projectCount: number, projectTitle: string) {
    if (projectCount === 0) {
      return sessionContextLabel();
    }

    if (projectCount === 1) {
      return projectTitle;
    }

    return `${formatCount(projectCount)} projects`;
  }

  async function loadShell(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      overview = await fetchOverview();
      syncCreateDefaults();
      authPromptVisible = false;
      authMessage = null;
      error = null;
    } catch (cause) {
      if (isAuthError(cause)) {
        authPromptVisible = true;
        authMessage = cause.message;
        error = null;
      } else {
        error = cause instanceof Error ? cause.message : 'Failed to reach Nucleus.';
      }
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function loadSettings() {
    try {
      applySettings(await fetchSettings());
    } catch (cause) {
      if (isAuthError(cause)) {
        authPromptVisible = true;
        authMessage = cause.message;
      }
    }
  }

  function prependSession(session: SessionSummary) {
    if (!overview) {
      return;
    }

    overview = {
      ...overview,
      sessions: [session, ...overview.sessions.filter((item) => item.id !== session.id)]
    };
  }

  async function handleCreateSession() {
    if (compatibilityBlocked || !createCanSubmit) {
      return;
    }

    creating = true;

    try {
      const trimmedTitle = createTitle.trim();
      const detail = await createSession({
        ...(trimmedTitle ? { title: trimmedTitle } : {}),
        attachment_mode: createAttachmentMode,
        ...(createAttachmentMode === 'scratch'
          ? {}
          : {
              primary_project_id: createProjectId,
              project_ids: [createProjectId]
            })
      });

      prependSession(detail.session);
      error = null;
      createDialogOpen = false;
      createTitle = '';
      sidebarOpen = false;
      await goto(`/?session=${detail.session.id}`, { noScroll: true });
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to create the session.';
    } finally {
      creating = false;
    }
  }

  async function openSession(sessionId: string) {
    sidebarOpen = false;
    await goto(`/?session=${sessionId}`, { noScroll: true });
  }

  async function openNavigation(href: string) {
    sidebarOpen = false;
    await goto(href);
  }

  function applySettings(next: SettingsSummary) {
    settings = next;
    daemonCompatibility = next.compatibility;
    syncUpdateToast(next);
  }

  function syncUpdateToast(next: SettingsSummary) {
    const targetId =
      next.update.latest_release_id ?? next.update.latest_version ?? next.update.latest_commit;

    if (
      !next.update.update_available ||
      next.update.restart_required ||
      next.update.last_attempt_result !== 'success' ||
      !targetId
    ) {
      updateToastVisible = false;
      return;
    }

    updateToastVisible = targetId !== dismissedUpdateTarget;
  }

  function dismissUpdateToast() {
    const targetId =
      settings?.update.latest_release_id ??
      settings?.update.latest_version ??
      settings?.update.latest_commit ??
      '';
    dismissedUpdateTarget = targetId;
    updateToastVisible = false;

    if (targetId) {
      window.localStorage.setItem('nucleus.dismissedUpdateTarget', targetId);
    }
  }

  async function handleSaveAccessToken() {
    authSubmitting = true;
    authMessage = null;
    writeAccessToken(authTokenInput);

    try {
      await fetchSettings();
      window.location.reload();
    } catch (cause) {
      if (isAuthError(cause)) {
        authPromptVisible = true;
        authMessage = cause.message;
      } else {
        error = cause instanceof Error ? cause.message : 'Failed to validate the access token.';
      }
    } finally {
      authSubmitting = false;
    }
  }

  function handleClearAccessToken() {
    clearAccessToken();
    authTokenInput = '';
    authPromptVisible = true;
    authMessage = 'Enter the Nucleus access token for this server.';
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event === 'connected') {
      daemonCompatibility = event.data.compatibility;

      if (settings) {
        settings = {
          ...settings,
          compatibility: event.data.compatibility
        };
      }

      return;
    }

    if (event.event === 'overview.updated') {
      overview = event.data;
      syncCreateDefaults();
      loading = false;
      refreshing = false;
      error = null;
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
    authTokenInput = readAccessToken();
    dismissedUpdateTarget = window.localStorage.getItem('nucleus.dismissedUpdateTarget') ?? '';
    void loadShell();
    void loadSettings();

    const disconnect = connectDaemonStream({
      onEvent: applyStreamEvent,
      onStatusChange: (status) => {
        streamStatus = status;
      },
      onError: (message) => {
        error = message;
      },
      onAuthError: (message) => {
        authPromptVisible = true;
        authMessage = message;
      }
    });

    return () => {
      disconnect();
    };
  });
</script>

<div class="flex h-dvh min-h-0 flex-col overflow-hidden bg-zinc-950 text-zinc-100 lg:grid lg:grid-cols-[minmax(0,16.5rem)_minmax(0,1fr)]">
  <AppSidebar
    open={sidebarOpen}
    {pathname}
    {navigation}
    {overview}
    {activeSidebarSessionId}
    {creating}
    {compatibilityBlocked}
    {createSessionTitle}
    {hasUpdateAvailable}
    {restartRequired}
    updateTrackLabel={updateTrackLabel}
    updateLastAttemptResult={updateStatus?.last_attempt_result ?? null}
    {projectLabel}
    {formatState}
    {badgeVariantForSession}
    {isNavActive}
    {openNavigation}
    {openCreateSessionDialog}
    closeSidebar={() => {
      sidebarOpen = false;
    }}
  />


  <Dialog bind:open={createDialogOpen}>
    <DialogContent class="w-[min(34rem,calc(100vw-2rem))]">
      <div class="flex items-start justify-between gap-4">
        <div class="min-w-0">
          <DialogTitle id="session-create-title">Start session</DialogTitle>
          <DialogDescription>Choose where this session should work before the composer opens.</DialogDescription>
        </div>
      </div>

      <form class="mt-5 space-y-5" onsubmit={(event) => { event.preventDefault(); void handleCreateSession(); }}>
        <div class="grid gap-2">
          {#each ATTACHMENT_MODE_OPTIONS as option}
            {@const OptionIcon = option.icon}
            <label
              class={cn(
                'flex cursor-pointer items-start gap-3 rounded-md border px-3 py-3 transition-colors',
                createAttachmentMode === option.value
                  ? 'border-lime-400/60 bg-lime-400/10'
                  : 'border-zinc-800 bg-zinc-950 hover:border-zinc-700 hover:bg-zinc-900'
              )}
            >
              <input
                class="sr-only"
                type="radio"
                name="attachment-mode"
                value={option.value}
                checked={createAttachmentMode === option.value}
                onchange={() => {
                  createAttachmentMode = option.value;
                  if (option.value !== 'scratch' && !createProjectId) {
                    createProjectId = discoveredProjects[0]?.id ?? '';
                  }
                }}
              />
              <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-zinc-900 text-zinc-300">
                <OptionIcon class="size-4" />
              </span>
              <span class="min-w-0">
                <span class="block text-sm font-medium text-zinc-100">{option.label}</span>
                <span class="mt-0.5 block text-xs leading-5 text-zinc-500">{option.description}</span>
              </span>
            </label>
          {/each}
        </div>

        {#if createRequiresProject}
          <div class="space-y-1.5">
            <Label for="session-create-project">Project</Label>
            <Select
              id="session-create-project"
              bind:value={createProjectId}
              disabled={discoveredProjects.length === 0}
            >
              {#if discoveredProjects.length === 0}
                <option value="">No projects discovered</option>
              {:else}
                {#each discoveredProjects as project}
                  <option value={project.id}>{project.title} · {project.relative_path}</option>
                {/each}
              {/if}
            </Select>
          </div>
        {/if}

        <div class="space-y-1.5">
          <Label for="session-create-title-input">Title</Label>
          <Input
            id="session-create-title-input"
            bind:value={createTitle}
            placeholder={createSessionTitle}
            autocomplete="off"
          />
        </div>

        {#if error}
          <div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-200">
            {error}
          </div>
        {/if}

        <div class="flex flex-wrap items-center justify-end gap-2">
          <Button type="button" variant="ghost" onclick={() => (createDialogOpen = false)}>
            Cancel
          </Button>
          <Button type="submit" disabled={creating || compatibilityBlocked || !createCanSubmit}>
            <MessageSquarePlus class={creating ? 'size-4 animate-spin' : 'size-4'} />
            {creating ? 'Starting...' : 'Start session'}
          </Button>
        </div>
      </form>
    </DialogContent>
  </Dialog>

  <main
    class={cn(
      'flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden',
      usesFullHeightContent ? 'px-0 py-0' : 'px-4 py-4 sm:px-6 lg:px-8 lg:py-6'
    )}
  >
    <div
      class={cn(
        'sticky top-0 z-20 border-b border-zinc-900 bg-zinc-950/95 px-4 py-2.5 backdrop-blur sm:px-6 lg:hidden',
        usesFullHeightContent ? 'shrink-0' : '-mx-4 mb-4 sm:-mx-6'
      )}
    >
      <div class="flex items-center justify-between gap-3">
        <div class="flex min-w-0 items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            aria-label="Open sidebar"
            title="Open sidebar"
            onclick={() => {
              sidebarOpen = true;
            }}
          >
            <Menu class="size-4" />
          </Button>
          <div class="truncate text-sm font-medium text-zinc-100">{activeNavItem.label}</div>
        </div>

        <div class="flex items-center gap-2">
          <Badge variant={error || compatibilityBlocked ? 'destructive' : 'default'}>{statusLabel}</Badge>
          <Button
            size="icon"
            variant="outline"
            class="h-10 w-10"
            aria-label={createSessionTitle}
            title={createSessionTitle}
            disabled={creating || compatibilityBlocked}
            onclick={openCreateSessionDialog}
          >
            <MessageSquarePlus class={creating ? 'size-4 animate-spin' : 'size-4'} />
          </Button>
        </div>
      </div>
    </div>

    {#if error && !authPromptVisible}
      <div class="mb-6 rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
        {error}
      </div>
    {/if}

    {#if compatibilityWarning}
      <div
        class={compatibilityBlocked
          ? 'mb-6 rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-100'
          : 'mb-6 rounded-lg border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100'}
      >
        {compatibilityWarning}
      </div>
    {/if}

    <div
      class={cn(
        'min-h-0 min-w-0 flex-1',
        usesFullHeightContent ? 'flex overflow-hidden' : 'overflow-y-auto'
      )}
    >
      <div
        class={cn(
          usesFullHeightContent ? 'flex min-h-0 min-w-0 flex-1 overflow-hidden' : 'pb-8'
        )}
      >
        {#if compatibilityBlocked}
          <section class="mx-auto flex min-h-[22rem] max-w-2xl flex-col justify-center py-12">
            <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-5 py-4">
              <div class="text-base font-semibold text-red-100">Incompatible Client</div>
              <p class="mt-2 text-sm leading-6 text-red-100/85">
                {compatibilityWarning}
              </p>
            </div>
          </section>
        {:else}
          {@render children()}
        {/if}
      </div>
    </div>
  </main>
</div>

{#if authPromptVisible}
  <div class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 px-4">
    <div class="w-full max-w-md rounded-lg border border-zinc-800 bg-zinc-950 p-5 shadow-2xl">
      <div class="text-lg font-semibold text-zinc-50">Connect To Nucleus</div>
      <div class="mt-2 text-sm leading-6 text-zinc-400">
        This server requires a bearer token before the Nucleus APIs and session stream become available.
      </div>

      <div class="mt-4 space-y-2">
        <label class="text-xs uppercase tracking-[0.16em] text-zinc-500" for="nucleus-access-token">
          Access Token
        </label>
        <input
          id="nucleus-access-token"
          class="h-11 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
          bind:value={authTokenInput}
          placeholder="nuctk_..."
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
        />
      </div>

      <div class="mt-3 rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-2 text-xs text-zinc-500">
        Current origin: {page.url.origin}
      </div>

      {#if authMessage}
        <div class="mt-3 rounded-md border border-amber-400/20 bg-amber-400/10 px-3 py-2 text-sm text-amber-100">
          {authMessage}
        </div>
      {/if}

      <div class="mt-5 flex items-center justify-end gap-2">
        <Button variant="ghost" onclick={handleClearAccessToken}>Clear</Button>
        <Button onclick={handleSaveAccessToken} disabled={authSubmitting || !authTokenInput.trim()}>
          {authSubmitting ? 'Checking...' : 'Connect'}
        </Button>
      </div>
    </div>
  </div>
{/if}

{#if updateToastVisible && settings}
  <div class="fixed bottom-4 right-4 z-50 w-[min(22rem,calc(100vw-2rem))] rounded-lg border border-lime-300/20 bg-zinc-950/95 px-4 py-3 shadow-2xl backdrop-blur">
    <div class="text-sm font-medium text-zinc-50">Update available</div>
    <div class="mt-1 text-xs leading-5 text-zinc-400">
      {updateTargetLabel} is available on {updateTrackLabel || 'the tracked target'}.
    </div>
    <div class="mt-3 flex items-center gap-2">
      <Button
        size="sm"
        onclick={() => {
          void openNavigation('/workspace/settings');
        }}
      >
        Open settings
      </Button>
      <Button size="sm" variant="ghost" onclick={dismissUpdateToast}>Dismiss</Button>
    </div>
  </div>
{/if}
