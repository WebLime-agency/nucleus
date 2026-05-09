<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLSelectAttributes } from 'svelte/elements';

  interface Props extends Omit<HTMLSelectAttributes, 'value'> {
    children: Snippet;
    ref?: HTMLSelectElement | null;
    value?: string;
  }

  let {
    ref = $bindable<HTMLSelectElement | null>(null),
    value = $bindable<string>(),
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<select
  bind:this={ref}
  bind:value
  class={cn(
    'flex h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none transition-colors focus:border-zinc-700 focus-visible:ring-2 focus-visible:ring-zinc-700/70 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  {...rest}
>
  {@render children()}
</select>
