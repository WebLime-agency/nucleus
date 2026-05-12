<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';
  interface Props extends HTMLAttributes<HTMLDivElement> { children: Snippet; }
  let { class: className, children, ...restProps }: Props = $props();
</script>
<div class={cn('flex flex-wrap justify-between gap-2 border-t border-zinc-800 p-4', className)} {...restProps}>{@render children()}</div>
