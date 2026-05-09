<script lang="ts">
  import SidebarSessionItem from './sidebar-session-item.svelte';

  type BadgeVariant = 'default' | 'secondary' | 'warning' | 'destructive';

  type SessionView = {
    id: string;
    title: string;
    projectLabel: string;
    turnCount: number;
    excerpt: string | null;
    stateLabel: string;
    stateVariant: BadgeVariant;
  };

  let {
    sessions,
    activeSessionId = null,
    onOpen
  }: {
    sessions: SessionView[];
    activeSessionId?: string | null;
    onOpen: (sessionId: string) => void;
  } = $props();
</script>

<div class="px-3 py-3">
  {#if sessions.length === 0}
    <div class="rounded-lg border border-zinc-900 bg-zinc-950 px-3 py-4 text-sm text-zinc-500">
      No sessions yet.
    </div>
  {:else}
    <div class="space-y-2">
      {#each sessions as session}
        <SidebarSessionItem
          title={session.title}
          projectLabel={session.projectLabel}
          turnCount={session.turnCount}
          excerpt={session.excerpt}
          stateLabel={session.stateLabel}
          stateVariant={session.stateVariant}
          active={session.id === activeSessionId}
          onclick={() => onOpen(session.id)}
        />
      {/each}
    </div>
  {/if}
</div>
