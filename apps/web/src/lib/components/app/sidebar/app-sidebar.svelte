<script lang="ts">
  import { FolderKanban, MessageSquarePlus, PencilLine, X } from 'lucide-svelte';

  import type { ProjectSummary, RuntimeOverview, SessionSummary } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';

  import { Button } from '$lib/components/ui/button';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';

  import SidebarFooter from './sidebar-footer.svelte';
  import SidebarSessionList from './sidebar-session-list.svelte';

  type WorkspaceMode = 'isolated_worktree' | 'shared_project_root' | 'scratch_only';

  type Props = {
    open: boolean;
    pathname: string;
    overview?: RuntimeOverview | null;
    navigation: { href: string; label: string; icon: typeof import('lucide-svelte').Icon }[];
    activeSidebarSessionId?: string;
    creating?: boolean;
    compatibilityBlocked?: boolean;
    createSessionTitle?: string;
    createProjectId?: string;
    createWorkspaceMode?: WorkspaceMode;
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
    onSelectCreateWorkspaceMode?: (mode: WorkspaceMode) => void;
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
    createWorkspaceMode = 'isolated_worktree',
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
    onSelectCreateWorkspaceMode = () => {},
    closeSidebar
  }: Props = $props();

  const WORKSPACE_MODE_OPTIONS: { value: WorkspaceMode; label: string; description: string }[] = [
    {
      value: 'isolated_worktree',
      label: 'New worktree',
      description: 'Create a separate worktree for this session.'
    },
    {
      value: 'shared_project_root',
      label: 'Use project root',
      description: 'Work directly in the main project checkout.'
    },
    {
      value: 'scratch_only',
      label: 'No project',
      description: 'Start in workspace scratch without a linked checkout.'
    }
  ];

  let sessions = $derived(overview?.sessions ?? []);
  let selectedCreateProject = $derived(
    projects.find((project) => project.id === createProjectId) ?? null
  );
  let currentModeOption = $derived(
    WORKSPACE_MODE_OPTIONS.find((option) => option.value === createWorkspaceMode) ??
      WORKSPACE_MODE_OPTIONS[0]
  );
  let projectMenuOpen = $state(false);

  function handleSelectProject(projectId: string) {
    onSelectCreateProject(projectId);
    projectMenuOpen = false;
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
  <div class="relative border-b border-zinc-900 px-3 py-3">
    <div class="space-y-2">
      <div class="flex items-center justify-between gap-2">
        <div class="truncate text-[1.875rem] font-semibold tracking-tight text-zinc-50" title="Nucleus">Nucleus</div>

        <div class="flex shrink-0 items-center gap-1">
          <div class="relative shrink-0">
            <DropdownMenu.Root bind:open={projectMenuOpen}>
              <DropdownMenu.Trigger
                class="relative inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-black text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700 disabled:pointer-events-none disabled:opacity-50"
                aria-label={selectedCreateProject ? `Project: ${selectedCreateProject.title}` : 'Choose project for new session'}
                title={selectedCreateProject ? `Project: ${selectedCreateProject.title}` : 'Choose project for new session'}
                disabled={creating || compatibilityBlocked}
              >
                <FolderKanban class="size-4" />
                {#if selectedCreateProject}
                  <span class="absolute right-1.5 top-1.5 h-2 w-2 rounded-full bg-lime-400"></span>
                {/if}
              </DropdownMenu.Trigger>
              <DropdownMenu.Content
                side="bottom"
                align="start"
                sideOffset={8}
                class="z-50 w-[calc(100vw-2rem)] min-w-[18rem] max-w-[calc(20rem-1.5rem)]"
              >
                <div class="max-h-[min(24rem,calc(100vh-9rem))] overflow-y-auto">
                  <button
                    type="button"
                    class="flex w-full items-start gap-3 rounded-md px-3 py-2 text-left transition-colors hover:bg-zinc-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700"
                    onclick={() => handleSelectProject('')}
                  >
                    <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                      <MessageSquarePlus class="size-4" />
                    </div>
                    <div class="min-w-0 flex-1">
                      <div class="flex items-center justify-between gap-3">
                        <span class="truncate text-sm font-medium text-zinc-100">Workspace scratch</span>
                        {#if !selectedCreateProject}
                          <span class="rounded-full bg-lime-400/15 px-2 py-0.5 text-[11px] font-medium text-lime-300">Selected</span>
                        {/if}
                      </div>
                      <div class="mt-0.5 text-xs leading-5 text-zinc-500">Start without an attached project.</div>
                    </div>
                  </button>

                  {#each projects as project}
                    <button
                      type="button"
                      class="flex w-full items-start gap-3 rounded-md px-3 py-2 text-left transition-colors hover:bg-zinc-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700"
                      onclick={() => handleSelectProject(project.id)}
                    >
                      <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                        <FolderKanban class="size-4" />
                      </div>
                      <div class="min-w-0 flex-1">
                        <div class="flex items-center justify-between gap-3">
                          <span class="truncate text-sm font-medium text-zinc-100">{project.title}</span>
                          {#if project.id === createProjectId}
                            <span class="rounded-full bg-lime-400/15 px-2 py-0.5 text-[11px] font-medium text-lime-300">Selected</span>
                          {/if}
                        </div>
                        <div class="mt-0.5 truncate text-xs leading-5 text-zinc-500">{project.relative_path}</div>
                      </div>
                    </button>
                  {/each}
                </div>
              </DropdownMenu.Content>
            </DropdownMenu.Root>
          </div>

          <div class="relative shrink-0">
            <DropdownMenu.Root>
              <DropdownMenu.Trigger
                class="relative inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-black text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700 disabled:pointer-events-none disabled:opacity-50"
                aria-label={`Session setup: ${currentModeOption.label}`}
                title={`Session setup: ${currentModeOption.label}`}
                disabled={creating || compatibilityBlocked}
              >
                {#if createWorkspaceMode === 'isolated_worktree'}
                  <FolderKanban class="size-4" />
                {:else if createWorkspaceMode === 'shared_project_root'}
                  <PencilLine class="size-4" />
                {:else}
                  <MessageSquarePlus class="size-4" />
                {/if}
                {#if createWorkspaceMode !== 'scratch_only'}
                  <span class="absolute right-1.5 top-1.5 h-2 w-2 rounded-full bg-lime-400"></span>
                {/if}
              </DropdownMenu.Trigger>
              <DropdownMenu.Content
                side="bottom"
                align="start"
                sideOffset={8}
                class="z-50 w-[calc(100vw-2rem)] min-w-[18rem] max-w-[calc(20rem-1.5rem)]"
              >
                <DropdownMenu.RadioGroup
                value={createWorkspaceMode}
                onValueChange={(value) => {
                  if (
                    value === 'isolated_worktree' ||
                    value === 'shared_project_root' ||
                    value === 'scratch_only'
                  ) {
                    onSelectCreateWorkspaceMode(value);
                  }
                }}
              >
                {#each WORKSPACE_MODE_OPTIONS as option}
                  <DropdownMenu.RadioItem value={option.value} class="items-start gap-3 py-2 pl-2 pr-8">
                    <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                      {#if option.value === 'isolated_worktree'}
                        <FolderKanban class="size-4" />
                      {:else if option.value === 'shared_project_root'}
                        <PencilLine class="size-4" />
                      {:else}
                        <MessageSquarePlus class="size-4" />
                      {/if}
                    </div>
                    <div class="min-w-0">
                      <div class="text-sm font-medium text-zinc-100">{option.label}</div>
                      <div class="mt-0.5 text-xs leading-5 text-zinc-500">{option.description}</div>
                    </div>
                  </DropdownMenu.RadioItem>
                {/each}
              </DropdownMenu.RadioGroup>
              </DropdownMenu.Content>
            </DropdownMenu.Root>
          </div>

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
  </div>

  <div class="flex min-h-0 flex-1 flex-col">
    <div class="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
      <div class="px-3 pt-3 text-[11px] font-medium uppercase tracking-[0.14em] text-zinc-600">
        Sessions
      </div>
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
