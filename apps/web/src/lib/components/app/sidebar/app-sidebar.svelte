<script lang="ts">
  import { cn } from '$lib/utils';
  import { Button } from '$lib/components/ui/button';
  import { fly } from 'svelte/transition';
  import type {
    ProjectSummary,
    RuntimeOverview,
    SettingsSummary,
    SessionSummary
  } from '$lib/nucleus/schemas';
  import type { StreamStatus } from '$lib/nucleus/realtime';
  import { MessageSquarePlus, MoreVertical, X } from '@lucide/svelte';
  import SidebarFooter from './sidebar-footer.svelte';
  import SidebarProjectPicker from './sidebar-project-picker.svelte';
  import SidebarSessionList from './sidebar-session-list.svelte';

  type NavItem = {
    href: string;
    label: string;
    icon: any;
  };

  type Props = {
    open: boolean;
    pathname: string;
    overview: RuntimeOverview | null;
    settings?: SettingsSummary | null;
    loading?: boolean;
    streamStatus?: StreamStatus;
    navigation: NavItem[];
    activeSidebarSessionId?: string;
    creating?: boolean;
    compatibilityBlocked?: boolean;
    createSessionTitle?: string;
    createProjectId?: string;
    projects?: ProjectSummary[];
    hasUpdateAvailable?: boolean;
    restartRequired?: boolean;
    updateTrackLabel?: string;
    updateLastAttemptResult?: string | null;
    projectLabel: (projectCount: number, projectTitle: string) => string;
    markdownExcerpt: (value: string) => string;
    formatState: (value: string) => string;
    badgeVariantForSession: (value: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    isNavActive: (href: string, currentPath: string) => boolean;
    openNavigation: (href: string) => void | Promise<void>;
    handleCreateSession: () => void | Promise<void>;
    onSelectCreateProject?: (projectId: string) => void;
    closeSidebar: () => void;
  };

  let {
    open,
    pathname,
    overview,
    navigation,
    activeSidebarSessionId = '',
    creating = false,
    compatibilityBlocked = false,
    createSessionTitle = '',
    createProjectId = '',
    projects = [],
    hasUpdateAvailable = false,
    restartRequired = false,
    updateTrackLabel = '',
    updateLastAttemptResult = null,
    projectLabel,
    markdownExcerpt,
    formatState,
    badgeVariantForSession,
    isNavActive,
    openNavigation,
    handleCreateSession,
    onSelectCreateProject = () => {},
    closeSidebar
  }: Props = $props();

  let sessions = $derived(overview?.sessions ?? []);
  let selectedCreateProject = $derived(
    projects.find((project) => project.id === createProjectId) ?? null
  );
  let projectPickerOpen = $state(false);

  function handleSelectProject(projectId: string) {
    onSelectCreateProject(projectId);
    projectPickerOpen = false;
  }
</script>

{#if open}
  <button
    type="button"
    class="fixed inset-0 z-30 bg-black/50 lg:hidden"
    aria-label="Close sidebar"
    onclick={closeSidebar}
  ></button>
{/if}

<aside
  class={cn(
    'fixed inset-y-0 left-0 z-40 flex min-h-0 w-80 min-w-0 max-w-[85vw] flex-col overflow-hidden border-r border-zinc-900 bg-zinc-950 transition-transform lg:static lg:z-auto lg:h-dvh lg:w-[16.5rem] lg:max-w-[16.5rem] lg:translate-x-0',
    open ? 'translate-x-0' : '-translate-x-full'
  )}
>
  <div class="border-b border-zinc-900 px-3 py-3">
    <div class="flex items-center justify-between gap-2">
      <div class="min-w-0 flex-1">
        <div class="truncate text-sm font-semibold text-zinc-100">Nucleus</div>
        <div class="mt-0.5 hidden truncate text-[11px] text-zinc-600 lg:block">
          {selectedCreateProject ? selectedCreateProject.title : 'Workspace scratch'}
        </div>
      </div>

      <div class="flex shrink-0 items-center gap-1">
        <Button
          variant="ghost"
          size="icon"
          class="h-9 w-9"
          disabled={creating || compatibilityBlocked}
          aria-label={projectPickerOpen ? 'Show sessions' : 'Choose project for new session'}
          title={projectPickerOpen ? 'Show sessions' : 'Choose project for new session'}
          aria-pressed={projectPickerOpen}
          onclick={() => {
            projectPickerOpen = !projectPickerOpen;
          }}
        >
          <MoreVertical class="size-4" />
        </Button>

        <Button
          size="icon"
          class="h-9 w-9"
          disabled={creating || compatibilityBlocked}
          title={createSessionTitle}
          aria-label={createSessionTitle || 'New session'}
          onclick={handleCreateSession}
        >
          <MessageSquarePlus class={creating ? 'size-4 animate-spin' : 'size-4'} />
        </Button>

        <Button variant="ghost" size="icon" class="h-9 w-9 lg:hidden" aria-label="Close sidebar" onclick={closeSidebar}>
          <X class="size-4" />
        </Button>
      </div>
    </div>
  </div>

  <div class="flex min-h-0 flex-1 flex-col">
    <div class="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
      <div class="flex items-center justify-between gap-2 px-3 pt-3">
        <div class="min-w-0 truncate text-[11px] font-medium uppercase tracking-[0.14em] text-zinc-600">
          {projectPickerOpen ? 'New Session Context' : 'Sessions'}
        </div>
        {#if projectPickerOpen}
          <Button
            variant="ghost"
            size="sm"
            class="h-7 px-2"
            onclick={() => {
              projectPickerOpen = false;
            }}
          >
            Sessions
          </Button>
        {/if}
      </div>

      {#key projectPickerOpen}
        <div
          in:fly={{ x: projectPickerOpen ? 12 : -12, duration: 140 }}
          out:fly={{ x: projectPickerOpen ? -12 : 12, duration: 100 }}
        >
          {#if projectPickerOpen}
            <SidebarProjectPicker
              {projects}
              selectedProjectId={createProjectId}
              onSelect={handleSelectProject}
            />
          {:else}
            <SidebarSessionList
              sessions={sessions.map((session: SessionSummary) => ({
                id: session.id,
                title: session.title,
                projectLabel: projectLabel(session.project_count, session.project_title),
                turnCount: session.turn_count,
                excerpt: session.last_message_excerpt ? markdownExcerpt(session.last_message_excerpt) : null,
                stateLabel: formatState(session.state),
                stateVariant: badgeVariantForSession(session.state)
              }))}
              activeSessionId={activeSidebarSessionId}
              onOpen={(sessionId) => openNavigation(`/?session=${sessionId}`)}
            />
          {/if}
        </div>
      {/key}
    </div>

    <SidebarFooter
      {navigation}
      {pathname}
      {isNavActive}
      {openNavigation}
      {hasUpdateAvailable}
      {restartRequired}
      {updateTrackLabel}
      {updateLastAttemptResult}
    />
  </div>
</aside>
