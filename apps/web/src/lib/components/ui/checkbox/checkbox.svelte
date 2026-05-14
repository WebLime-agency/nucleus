<script lang="ts">
  import { cn } from '$lib/utils';
  import type { HTMLInputAttributes } from 'svelte/elements';

  interface Props extends Omit<HTMLInputAttributes, 'type' | 'checked'> {
    ref?: HTMLInputElement | null;
    checked?: boolean;
  }

  let {
    ref = $bindable<HTMLInputElement | null>(null),
    checked = $bindable(false),
    class: className,
    ...rest
  }: Props = $props();
</script>

<input
  bind:this={ref}
  bind:checked
  type="checkbox"
  class={cn(
    'size-4 rounded border border-zinc-700 bg-zinc-950 text-lime-300 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-zinc-700/70 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  {...rest}
/>
