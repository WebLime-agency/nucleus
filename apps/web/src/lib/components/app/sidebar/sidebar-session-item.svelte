<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { cn } from '$lib/utils';

  type BadgeVariant = 'default' | 'secondary' | 'warning' | 'destructive';

  type Props = {
    title: string;
    projectLabel: string;
    turnCount: number;
    excerpt?: string | null;
    stateLabel: string;
    stateVariant: BadgeVariant;
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
    active = false,
    onclick
  }: Props = $props();
</script>

<button
  type="button"
  class={cn(
    'w-full rounded-lg border px-3 py-3 text-left transition-colors',
    active ? 'border-lime-300/30 bg-lime-300/10' : 'border-zinc-900 bg-zinc-950 hover:bg-zinc-900'
  )}
  {onclick}
>
  <div class="flex items-start justify-between gap-3">
    <div class="min-w-0">
      <div class="truncate text-sm font-medium text-zinc-100">{title}</div>
      <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-zinc-500">
        <span>{projectLabel}</span>
        <span>{turnCount} turns</span>
      </div>
      {#if excerpt}
        <div class="mt-1 line-clamp-1 text-xs leading-5 text-zinc-400">{excerpt}</div>
      {/if}
    </div>
    <Badge variant={stateVariant} class="shrink-0">{stateLabel}</Badge>
  </div>
</button>
