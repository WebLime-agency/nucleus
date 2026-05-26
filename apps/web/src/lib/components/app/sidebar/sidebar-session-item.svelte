<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { formatRelativeTime } from '$lib/nucleus/format';
  import { cn } from '$lib/utils';

  type BadgeVariant = 'default' | 'secondary' | 'warning' | 'destructive';

  type Props = {
    title: string;
    projectLabel: string;
    stateLabel: string;
    stateVariant: BadgeVariant;
    state: string;
    created_at: number;
    last_resumed_at?: number | null;
    active?: boolean;
    onclick?: () => void;
  };

  let {
    title,
    projectLabel,
    stateLabel,
    stateVariant,
    state,
    created_at,
    last_resumed_at = null,
    active = false,
    onclick
  }: Props = $props();

  let lastActivityLabel = $derived(formatRelativeTime(last_resumed_at ?? created_at));
</script>

<button
  type="button"
  data-state={state}
  class={cn(
    'w-full min-w-0 overflow-hidden rounded-lg border px-3 py-3 text-left transition-colors',
    active ? 'border-lime-300/30 bg-lime-300/10' : 'border-zinc-900 bg-zinc-950 hover:bg-zinc-900'
  )}
  {onclick}
>
  <div class="flex items-start justify-between gap-3">
    <div class="min-w-0 flex-1">
      <div class="truncate text-sm font-medium text-zinc-100">{title}</div>
      <div class="mt-1 flex min-w-0 items-center gap-1.5 text-[11px] text-zinc-500">
        <span class="min-w-0 max-w-full truncate">{projectLabel}</span>
        <span class="shrink-0 text-zinc-700">·</span>
        <span class="shrink-0">{lastActivityLabel}</span>
      </div>
    </div>
    <Badge variant={stateVariant} class="shrink-0">{stateLabel}</Badge>
  </div>
</button>
