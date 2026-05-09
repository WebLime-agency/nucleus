<script lang="ts">
  import { cn } from '$lib/utils';
  import { Button } from '$lib/components/ui/button';
  import type { RuntimeOverview, SettingsSummary, SessionSummary } from '$lib/nucleus/schemas';
  import type { StreamStatus } from '$lib/nucleus/realtime';
  import { MessageSquarePlus, X } from '@lucide/svelte';
  import SidebarFooter from './sidebar-footer.svelte';
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
    hasUpdateAvailable?: boolean;
    restartRequired?: boolean;
    updateTrackLabel?: string;
    updateLastAttemptResult?: string | null;
    sessionsWithProjects?: number;
    formatCount: (value: number) => string;
    compactPath: (value: string) => string;
    projectLabel: (projectCount: number, projectTitle: string) => string;
    markdownExcerpt: (value: string) => string;
    formatState: (value: string) => string;
    badgeVariantForSession: (value: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    isNavActive: (href: string, currentPath: string) => boolean;
    openNavigation: (href: string) => void | Promise<void>;
    handleCreateSession: () => void | Promise<void>;
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
    hasUpdateAvailable = false,
    restartRequired = false,
    updateTrackLabel = '',
    updateLastAttemptResult = null,
    sessionsWithProjects = 0,
    formatCount,
    compactPath,
    projectLabel,
    markdownExcerpt,
    formatState,
    badgeVariantForSession,
    isNavActive,
    openNavigation,
    handleCreateSession,
    closeSidebar
  }: Props = $props();

  let workspace = $derived(overview?.workspace ?? null);
  let sessions = $derived(overview?.sessions ?? []);
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
    'fixed inset-y-0 left-0 z-40 flex w-80 max-w-[85vw] flex-col border-r border-zinc-900 bg-zinc-950 transition-transform lg:static lg:z-auto lg:w-auto lg:max-w-none lg:translate-x-0',
    open ? 'translate-x-0' : '-translate-x-full'
  )}
>
  <div class="flex items-center justify-between border-b border-zinc-900 px-3 py-3">
    <div>
      <div class="text-sm font-semibold text-zinc-100">Nucleus</div>
      <div class="mt-0.5 hidden text-[11px] text-zinc-600 lg:block">
        Local AI control plane
      </div>
    </div>
    <Button variant="ghost" size="icon" class="lg:hidden" aria-label="Close sidebar" onclick={closeSidebar}>
      <X class="size-4" />
    </Button>
  </div>

  <div class="flex min-h-0 flex-1 flex-col">
    <div class="border-b border-zinc-900 px-3 py-3">
      <Button
        class="w-full justify-start gap-2"
        disabled={creating || compatibilityBlocked}
        title={createSessionTitle}
        onclick={handleCreateSession}
      >
        <MessageSquarePlus class={creating ? 'size-4 animate-spin' : 'size-4'} />
        <span>New session</span>
      </Button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto">
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
      {workspace}
      {sessionsWithProjects}
      {formatCount}
      {compactPath}
    />
  </div>
</aside>
