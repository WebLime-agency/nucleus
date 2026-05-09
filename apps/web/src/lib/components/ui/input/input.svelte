<script lang="ts">
  import { cn } from '$lib/utils';
  import type { HTMLInputAttributes } from 'svelte/elements';

  type InputValue = string | number | readonly string[] | undefined;

  interface Props extends Omit<HTMLInputAttributes, 'value'> {
    ref?: HTMLInputElement | null;
    value?: InputValue;
  }

  let {
    ref = $bindable<HTMLInputElement | null>(null),
    value = $bindable<InputValue>(),
    class: className,
    ...rest
  }: Props = $props();
</script>

<input
  bind:this={ref}
  bind:value
  class={cn(
    'flex h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none transition-colors placeholder:text-zinc-600 focus:border-zinc-700 focus-visible:ring-2 focus-visible:ring-zinc-700/70 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  {...rest}
/>
