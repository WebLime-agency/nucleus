<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { cn } from '$lib/utils';

  type Props = {
    title: string;
    description: string;
    selected?: boolean;
    onclick?: () => void;
  };

  let { title, description, selected = false, onclick }: Props = $props();
</script>

<button
  type="button"
  class={cn(
    'w-full min-w-0 overflow-hidden rounded-lg border px-3 py-3 text-left transition-colors',
    selected
      ? 'border-lime-300/30 bg-lime-300/10'
      : 'border-zinc-900 bg-zinc-950 hover:bg-zinc-900'
  )}
  {onclick}
>
  <div class="flex items-start justify-between gap-3">
    <div class="min-w-0 flex-1">
      <div class="truncate text-sm font-medium text-zinc-100">{title}</div>
      <div class="mt-1 truncate text-[11px] text-zinc-500">{description}</div>
    </div>
    {#if selected}
      <Badge variant="default" class="shrink-0">Selected</Badge>
    {/if}
  </div>
</button>
