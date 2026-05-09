<script lang="ts">
  import { cn } from '$lib/utils';

  type Props = {
    label: string;
    active?: boolean;
    title?: string;
    icon?: any;
    onclick?: () => void;
    badge?: 'lime' | 'amber' | null;
    compact?: boolean;
  };

  let {
    label,
    active = false,
    title = label,
    icon,
    onclick,
    badge = null,
    compact = false
  }: Props = $props();
  let Icon = $derived(icon);
</script>

<button
  type="button"
  aria-current={active ? 'page' : undefined}
  aria-label={label}
  title={title}
  class={cn(
    'relative inline-flex h-9 w-full items-center rounded-md border text-sm transition-colors',
    compact ? 'justify-center gap-1.5 px-2' : 'justify-start gap-2 px-2.5',
    active
      ? 'border-lime-300/30 bg-lime-300/10 text-lime-100'
      : 'border-zinc-800 bg-zinc-950 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
  )}
  {onclick}
>
  {#if Icon}
    <Icon class="size-4 shrink-0" />
  {/if}
  {#if !compact}
    <span class="min-w-0 truncate">{label}</span>
  {/if}
  {#if badge}
    <span
      class={cn(
        compact ? 'absolute right-1.5 top-1.5 h-1.5 w-1.5 rounded-full' : 'ml-auto h-2 w-2 shrink-0 rounded-full',
        badge === 'amber' ? 'bg-amber-300' : 'bg-lime-300'
      )}
    ></span>
  {/if}
</button>
