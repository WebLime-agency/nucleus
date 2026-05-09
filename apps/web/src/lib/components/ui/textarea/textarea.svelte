<script lang="ts">
  import { cn } from '$lib/utils';
  import type { HTMLTextareaAttributes } from 'svelte/elements';

  interface Props extends Omit<HTMLTextareaAttributes, 'value'> {
    ref?: HTMLTextAreaElement | null;
    value?: string;
  }

  let {
    ref = $bindable<HTMLTextAreaElement | null>(null),
    value = $bindable<string>(),
    class: className,
    ...rest
  }: Props = $props();
</script>

<textarea
  bind:this={ref}
  bind:value
  class={cn(
    'flex min-h-20 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm leading-5 text-zinc-100 outline-none transition-colors placeholder:text-zinc-600 focus:border-zinc-700 focus-visible:ring-2 focus-visible:ring-zinc-700/70 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  {...rest}
></textarea>
