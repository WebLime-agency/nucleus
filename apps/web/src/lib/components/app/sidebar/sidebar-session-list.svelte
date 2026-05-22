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
    state: string;
    created_at: number;
    last_resumed_at?: number | null;
    last_reasoning?: string;
    last_reasoning_at?: number | null;
    token_usage_known?: boolean;
    prompt_tokens?: number;
    completion_tokens?: number;
    cached_tokens?: number;
    cost_usd_estimate?: number | null;
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
          state={session.state}
          created_at={session.created_at}
          last_resumed_at={session.last_resumed_at}
          last_reasoning={session.last_reasoning}
          last_reasoning_at={session.last_reasoning_at}
          token_usage_known={session.token_usage_known}
          prompt_tokens={session.prompt_tokens}
          completion_tokens={session.completion_tokens}
          cached_tokens={session.cached_tokens}
          cost_usd_estimate={session.cost_usd_estimate}
          active={session.id === activeSessionId}
          onclick={() => onOpen(session.id)}
        />
      {/each}
    </div>
  {/if}
</div>
