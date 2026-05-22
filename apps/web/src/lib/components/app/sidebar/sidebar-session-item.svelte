<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import ReasoningActivity from '$lib/components/app/session/reasoning-activity.svelte';
  import RuntimeBadge from '$lib/components/app/session/runtime-badge.svelte';
  import { usageView } from '$lib/nucleus/session-ux.js';
  import { cn } from '$lib/utils';

  type BadgeVariant = 'default' | 'secondary' | 'warning' | 'destructive';

  type Props = {
    title: string;
    projectLabel: string;
    turnCount: number;
    excerpt?: string | null;
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
    active?: boolean;
    onclick?: () => void;
  };

  let {
    title,
    projectLabel,
    turnCount,
    excerpt = null,
    stateLabel,
    stateVariant,
    state,
    created_at,
    last_resumed_at = null,
    last_reasoning = '',
    last_reasoning_at = null,
    token_usage_known = false,
    prompt_tokens = 0,
    completion_tokens = 0,
    cached_tokens = 0,
    cost_usd_estimate = null,
    active = false,
    onclick
  }: Props = $props();

  let observabilityRecord = $derived({
    state,
    created_at,
    last_resumed_at,
    last_reasoning,
    last_reasoning_at,
    token_usage_known,
    prompt_tokens,
    completion_tokens,
    cached_tokens,
    cost_usd_estimate
  });
  let usage = $derived(usageView(observabilityRecord));
</script>

<button
  type="button"
  class={cn(
    'w-full min-w-0 overflow-hidden rounded-lg border px-3 py-3 text-left transition-colors',
    active ? 'border-lime-300/30 bg-lime-300/10' : 'border-zinc-900 bg-zinc-950 hover:bg-zinc-900'
  )}
  {onclick}
>
  <div class="flex items-start justify-between gap-3">
    <div class="min-w-0 flex-1">
      <div class="truncate text-sm font-medium text-zinc-100">{title}</div>
      <div class="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-zinc-500">
        <span class="min-w-0 max-w-full truncate">{projectLabel}</span>
        <span>{turnCount} turns</span>
        <RuntimeBadge record={observabilityRecord} />
        <span title={usage.title}>{usage.label}</span>
      </div>
      <ReasoningActivity record={observabilityRecord} />
      {#if excerpt}
        <div class="mt-1 line-clamp-1 text-xs leading-5 text-zinc-400">{excerpt}</div>
      {/if}
    </div>
    <Badge variant={stateVariant} class="shrink-0">{stateLabel}</Badge>
  </div>
</button>
