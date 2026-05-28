<script lang="ts">
  import { MessageSquarePlus, X } from 'lucide-svelte';

  import type { RuntimeOverview, SessionSummary } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';

  import { Button } from '$lib/components/ui/button';

  import SidebarFooter from './sidebar-footer.svelte';
  import SidebarSessionList from './sidebar-session-list.svelte';

  type Props = {
    open: boolean;
    pathname: string;
    overview?: RuntimeOverview | null;
    navigation: { href: string; label: string; icon: typeof import('lucide-svelte').Icon }[];
    activeSidebarSessionId?: string;
    creating?: boolean;
    compatibilityBlocked?: boolean;
    createSessionTitle?: string;
    hasUpdateAvailable?: boolean;
    restartRequired?: boolean;
    updateTrackLabel?: string;
    updateLastAttemptResult?: string | null;
    projectLabel: (projectCount: number, projectTitle: string) => string;
    formatState: (value: string) => string;
    badgeVariantForSession: (value: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    isNavActive: (href: string, currentPath: string) => boolean;
    openNavigation: (href: string) => void | Promise<void>;
    openCreateSessionDialog: () => void;
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
    projectLabel,
    formatState,
    badgeVariantForSession,
    isNavActive,
    openNavigation,
    openCreateSessionDialog,
    closeSidebar
  }: Props = $props();

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
    'fixed inset-y-0 left-0 z-40 flex min-h-0 w-80 min-w-0 max-w-[85vw] flex-col overflow-hidden border-r border-zinc-900 bg-zinc-950 transition-transform lg:static lg:z-auto lg:h-dvh lg:w-[16.5rem] lg:max-w-[16.5rem] lg:translate-x-0',
    open ? 'translate-x-0' : '-translate-x-full'
  )}
>
  <div class="relative border-b border-zinc-900 px-3 py-3">
    <div class="space-y-2">
      <div class="flex items-center justify-between gap-2">
        <div class="truncate text-[1.875rem] font-semibold tracking-tight text-zinc-50" title="Nucleus">Nucleus</div>

        <div class="flex shrink-0 items-center gap-1">
          <Button
            size="icon"
            class="h-9 w-9"
            disabled={creating || compatibilityBlocked}
            title={createSessionTitle}
            aria-label={createSessionTitle || 'New session'}
            onclick={openCreateSessionDialog}
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
            stateLabel: formatState(session.state),
            stateVariant: badgeVariantForSession(session.state),
            state: session.state,
            created_at: session.created_at,
            last_resumed_at: session.last_resumed_at
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
