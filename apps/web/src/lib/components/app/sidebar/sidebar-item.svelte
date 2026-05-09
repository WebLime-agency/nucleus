<script lang="ts">
  import { cn } from '$lib/utils';

  type Props = {
    label: string;
    active?: boolean;
    title?: string;
    icon?: any;
    onclick?: () => void;
    badge?: 'lime' | 'amber' | null;
  };

  let { label, active = false, title = label, icon, onclick, badge = null }: Props = $props();
  let Icon = $derived(icon);
</script>

<button
  type="button"
  aria-current={active ? 'page' : undefined}
  aria-label={label}
  title={title}
  class={cn(
    'relative inline-flex h-10 items-center justify-center rounded-md border transition-colors',
    active
      ? 'border-lime-300/30 bg-lime-300/10 text-lime-100'
      : 'border-zinc-800 bg-zinc-950 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
  )}
  {onclick}
>
  {#if Icon}
    <Icon class="size-4" />
  {/if}
  {#if badge}
    <span
      class={cn(
        'absolute right-2 top-2 h-2 w-2 rounded-full',
        badge === 'amber' ? 'bg-amber-300' : 'bg-lime-300'
      )}
    ></span>
  {/if}
</button>
