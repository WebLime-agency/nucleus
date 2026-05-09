<script lang="ts">
  import { cn } from '$lib/utils';
  import { Button } from '$lib/components/ui/button';
  import type {
    ProjectSummary,
    RuntimeOverview,
    SettingsSummary,
    SessionSummary
  } from '$lib/nucleus/schemas';
  import type { StreamStatus } from '$lib/nucleus/realtime';
  import { MessageSquarePlus, X } from '@lucide/svelte';
  import SidebarFooter from './sidebar-footer.svelte';
  import SidebarProjectList from './sidebar-project-list.svelte';
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
  let projectListOpen = $state(false);

  function handleSelectProject(projectId: string) {
    onSelectCreateProject(projectId);
    projectListOpen = false;
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
        <button
          type="button"
          class={cn(
            'mt-0.5 block max-w-full truncate text-left text-[11px] transition-colors hover:text-zinc-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700 disabled:pointer-events-none disabled:opacity-50',
            selectedCreateProject ? 'font-medium text-lime-300' : 'text-zinc-600',
            projectListOpen && 'text-zinc-200'
          )}
          aria-expanded={projectListOpen}
          aria-label="Choose project for new session"
          title="Choose project for new session"
          disabled={creating || compatibilityBlocked}
          onclick={() => {
            projectListOpen = !projectListOpen;
          }}
        >
          {selectedCreateProject ? selectedCreateProject.title : 'Workspace scratch'}
        </button>
      </div>

      <div class="flex shrink-0 items-center gap-1">
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
      <div class="px-3 pt-3 text-[11px] font-medium uppercase tracking-[0.14em] text-zinc-600">
        {projectListOpen ? 'Projects' : 'Sessions'}
      </div>
      {#if projectListOpen}
        <SidebarProjectList
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
