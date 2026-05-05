<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import {
    Cpu,
    FolderRoot,
    FolderTree,
    Gauge,
    Menu,
    MemoryStick,
    MessageSquarePlus,
    MessagesSquare,
    Settings2,
    ServerCog,
    X
  } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    clearAccessToken,
    isAuthError,
    readAccessToken,
    writeAccessToken
  } from '$lib/nucleus/auth';
  import { createSession, fetchOverview, fetchSettings } from '$lib/nucleus/client';
  import { compactPath, formatCount, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    RuntimeOverview,
    SessionSummary,
    SettingsSummary
  } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';
  import '../app.css';

  let { children } = $props();

  const navigation = [
    { href: '/', label: 'Overview', icon: Gauge },
    { href: '/sessions', label: 'Sessions', icon: MessagesSquare },
    { href: '/workspace', label: 'Workspace', icon: FolderTree },
    { href: '/cpu', label: 'CPU', icon: Cpu },
    { href: '/memory', label: 'Memory', icon: MemoryStick },
    { href: '/settings', label: 'Settings', icon: Settings2 }
  ];

  let overview = $state<RuntimeOverview | null>(null);
  let settings = $state<SettingsSummary | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let creating = $state(false);
  let error = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');
  let createProjectId = $state('');
  let sidebarOpen = $state(false);
  let updateToastVisible = $state(false);
  let dismissedUpdateCommit = $state('');
  let authPromptVisible = $state(false);
  let authTokenInput = $state('');
  let authSubmitting = $state(false);
  let authMessage = $state<string | null>(null);

  let pathname = $derived(page.url.pathname);
  let workspace = $derived(overview?.workspace ?? null);
  let discoveredProjects = $derived(workspace?.projects ?? []);
  let sessions = $derived(overview?.sessions ?? []);
  let instanceName = $derived(settings?.instance.name ?? 'Nucleus');
  let updateStatus = $derived(settings?.update ?? null);
  let hasUpdateAvailable = $derived(updateStatus?.update_available ?? false);
  let restartRequired = $derived(updateStatus?.restart_required ?? false);
  let activeNavItem = $derived(navigation.find((item) => item.href === pathname) ?? navigation[0]);
  let usesFullHeightContent = $derived(pathname === '/sessions');
  let sessionsWithProjects = $derived(
    overview?.sessions.filter((session) => session.project_count > 0).length ?? 0
  );
  let requestedSessionId = $derived(page.url.searchParams.get('session') ?? '');
  let activeSidebarSessionId = $derived(
    pathname === '/sessions' ? requestedSessionId || sessions[0]?.id || '' : ''
  );
  let createSessionTitle = $derived.by(() => {
    if (!createProjectId) {
      return 'New session';
    }

    const project = discoveredProjects.find((item) => item.id === createProjectId);

    if (!project) {
      return 'New session';
    }

    return `New session from ${project.title}`;
  });
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (authPromptVisible) return 'Auth required';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (error) return 'Degraded';
    return 'Live';
  });

  function syncCreateDefaults() {
    if (!discoveredProjects.some((project) => project.id === createProjectId)) {
      createProjectId = '';
    }
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

  function createSessionContextLabel() {
    return 'Scratch';
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
        error = cause instanceof Error ? cause.message : 'Failed to reach the daemon.';
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
    creating = true;

    try {
      const detail = await createSession(
        createProjectId
          ? {
              primary_project_id: createProjectId,
              project_ids: [createProjectId]
            }
          : {}
      );

      prependSession(detail.session);
      error = null;
      sidebarOpen = false;
      await goto(`/sessions?session=${detail.session.id}`, { noScroll: true });
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to create the session.';
    } finally {
      creating = false;
    }
  }

  async function openSession(sessionId: string) {
    sidebarOpen = false;
    await goto(`/sessions?session=${sessionId}`, { noScroll: true });
  }

  async function openNavigation(href: string) {
    sidebarOpen = false;
    await goto(href);
  }

  function applySettings(next: SettingsSummary) {
    settings = next;
    syncUpdateToast(next);
  }

  function syncUpdateToast(next: SettingsSummary) {
    const remoteCommit = next.update.remote_commit;

    if (!next.update.update_available || next.update.restart_required || !remoteCommit) {
      updateToastVisible = false;
      return;
    }

    updateToastVisible = remoteCommit !== dismissedUpdateCommit;
  }

  function dismissUpdateToast() {
    const remoteCommit = settings?.update.remote_commit ?? '';
    dismissedUpdateCommit = remoteCommit;
    updateToastVisible = false;

    if (remoteCommit) {
      window.localStorage.setItem('nucleus.dismissedUpdateCommit', remoteCommit);
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
    dismissedUpdateCommit = window.localStorage.getItem('nucleus.dismissedUpdateCommit') ?? '';
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

<div class="h-screen overflow-hidden bg-zinc-950 text-zinc-100 lg:grid lg:grid-cols-[16.5rem_minmax(0,1fr)]">
  {#if sidebarOpen}
    <button
      type="button"
      class="fixed inset-0 z-30 bg-black/60 lg:hidden"
      aria-label="Close sidebar"
      onclick={() => {
        sidebarOpen = false;
      }}
    ></button>
  {/if}

  <aside
    class={cn(
      'fixed inset-y-0 left-0 z-40 flex h-screen w-[17rem] max-w-[86vw] flex-col overflow-hidden border-r border-zinc-900 bg-zinc-950 transition-transform lg:static lg:z-auto lg:w-auto lg:max-w-none lg:translate-x-0',
      sidebarOpen ? 'translate-x-0' : '-translate-x-full'
    )}
  >
    <div class="flex min-h-0 flex-1 flex-col">
      <div class="border-b border-zinc-900 px-3 py-3">
        <div class="flex items-center justify-between gap-3">
          <div class="flex min-w-0 items-center gap-3">
            <div class="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-zinc-950">
              <ServerCog class="size-4.5 text-lime-300/80" />
            </div>
            <div class="truncate text-base font-semibold text-zinc-50">{instanceName}</div>
          </div>

          <div class="flex items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              class="h-10 w-10 lg:hidden"
              aria-label="Close sidebar"
              title="Close sidebar"
              onclick={() => {
                sidebarOpen = false;
              }}
            >
              <X class="size-4" />
            </Button>
          </div>
        </div>
      </div>

      <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
        <div class="border-b border-zinc-900 px-3 py-2.5">
          <div class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-3">
            <div class="flex items-center gap-2 text-sm font-medium text-zinc-100">
              <MessagesSquare class="size-4 text-zinc-500" />
              <span>Sessions</span>
            </div>

            <div class="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
              <select
                class="h-10 min-w-0 rounded-md border border-zinc-800 bg-zinc-950 px-2.5 text-xs text-zinc-300 outline-none focus:border-zinc-700"
                bind:value={createProjectId}
                aria-label="Project for new session"
                title="Project for new session"
              >
                <option value="">{createSessionContextLabel()}</option>
                {#each discoveredProjects as project}
                  <option value={project.id}>{project.title}</option>
                {/each}
              </select>
              <Button
                size="icon"
                variant="outline"
                class="h-10 w-10 shrink-0"
                onclick={handleCreateSession}
                disabled={creating}
                aria-label={createSessionTitle}
                title={createSessionTitle}
              >
                <MessageSquarePlus class={creating ? 'size-4 animate-spin' : 'size-4'} />
              </Button>
            </div>
          </div>
        </div>

        <div class="min-h-0 flex-1 overflow-y-auto px-2.5 py-2.5">
          {#if sessions.length === 0}
            <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-3 text-sm text-zinc-500">
              No sessions yet.
            </div>
          {:else}
            <div class="space-y-2">
              {#each sessions as session}
                <button
                  type="button"
                  class={cn(
                    'w-full rounded-md border px-3 py-2 text-left transition-colors',
                    activeSidebarSessionId === session.id
                      ? 'border-lime-300/40 bg-lime-300/8'
                      : 'border-zinc-800 bg-zinc-950/40 hover:bg-zinc-900/80'
                  )}
                  title={session.title}
                  onclick={() => openSession(session.id)}
                >
                  <div class="flex items-start justify-between gap-3">
                    <div class="min-w-0">
                      <div class="truncate text-sm font-medium text-zinc-100">{session.title}</div>
                      <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-zinc-500">
                        <span>{projectLabel(session.project_count, session.project_title)}</span>
                        <span>{session.turn_count} turns</span>
                      </div>
                      {#if session.last_message_excerpt}
                        <div class="mt-1 line-clamp-1 text-xs leading-5 text-zinc-400">
                          {session.last_message_excerpt}
                        </div>
                      {/if}
                    </div>
                    <Badge variant={badgeVariantForSession(session.state)} class="shrink-0">
                      {formatState(session.state)}
                    </Badge>
                  </div>
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>

      <div class="sticky bottom-0 shrink-0 border-t border-zinc-900 bg-zinc-950/95 px-3 py-2.5 backdrop-blur">
        <nav class="grid grid-cols-6 gap-2">
          {#each navigation as item}
            <button
              type="button"
              aria-current={pathname === item.href ? 'page' : undefined}
              aria-label={item.label}
              title={item.label}
              class={cn(
                'relative inline-flex h-10 items-center justify-center rounded-md border transition-colors',
                pathname === item.href
                  ? 'border-lime-300/30 bg-lime-300/10 text-lime-100'
                  : 'border-zinc-800 bg-zinc-950 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
              )}
              onclick={() => openNavigation(item.href)}
            >
              <item.icon class="size-4" />
              {#if item.href === '/settings' && (hasUpdateAvailable || restartRequired)}
                <span
                  class={cn(
                    'absolute right-2 top-2 h-2 w-2 rounded-full',
                    restartRequired ? 'bg-amber-300' : 'bg-lime-300'
                  )}
                ></span>
              {/if}
            </button>
          {/each}
        </nav>

        <div class="mt-3 flex items-center gap-2 text-xs text-zinc-500">
          <FolderRoot class="size-3.5 text-zinc-600" />
          <span>{workspace ? formatCount(workspace.projects.length) : '0'} projects</span>
          {#if workspace}
            <span>-</span>
            <span>{formatCount(sessionsWithProjects)} attached</span>
          {/if}
        </div>
        {#if workspace}
          <div class="mt-1 truncate text-[11px] text-zinc-600" title={workspace.root_path}>
            {compactPath(workspace.root_path)}
          </div>
        {/if}
        {#if restartRequired}
          <div class="mt-2 text-[11px] text-amber-300/80">Restart required to load the latest update.</div>
        {:else if hasUpdateAvailable}
          <div class="mt-2 text-[11px] text-lime-300/80">Update available.</div>
        {/if}
      </div>
    </div>
  </aside>

  <main class="flex min-h-0 min-w-0 flex-col overflow-hidden px-4 py-4 sm:px-6 lg:px-8 lg:py-6">
    <div class="sticky top-0 z-20 -mx-4 mb-4 border-b border-zinc-900 bg-zinc-950/95 px-4 py-2.5 backdrop-blur sm:-mx-6 sm:px-6 lg:hidden">
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
          <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
          <Button
            size="icon"
            variant="outline"
            class="h-10 w-10"
            aria-label={createSessionTitle}
            title={createSessionTitle}
            disabled={creating}
            onclick={handleCreateSession}
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

    <div class={cn('min-h-0 flex-1', usesFullHeightContent ? 'overflow-hidden' : 'overflow-y-auto')}>
      <div class={cn(!usesFullHeightContent && 'pb-8')}>
        {@render children()}
      </div>
    </div>
  </main>
</div>

{#if authPromptVisible}
  <div class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 px-4">
    <div class="w-full max-w-md rounded-lg border border-zinc-800 bg-zinc-950 p-5 shadow-2xl">
      <div class="text-lg font-semibold text-zinc-50">Connect To Nucleus</div>
      <div class="mt-2 text-sm leading-6 text-zinc-400">
        This server requires a bearer token before the daemon APIs and session stream become available.
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
      {settings.update.remote_commit_short || 'A newer build'} is available on {settings.update.branch || 'main'}.
    </div>
    <div class="mt-3 flex items-center gap-2">
      <Button
        size="sm"
        onclick={() => {
          void openNavigation('/settings');
        }}
      >
        Open settings
      </Button>
      <Button size="sm" variant="ghost" onclick={dismissUpdateToast}>Dismiss</Button>
    </div>
  </div>
{/if}
