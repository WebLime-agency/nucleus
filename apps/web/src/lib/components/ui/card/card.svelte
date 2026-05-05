<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLDivElement> {
    children: Snippet;
  }

  let { class: className, children, ...rest }: Props = $props();
</script>

<div
  class={cn('min-w-0 rounded-lg border border-zinc-800 bg-zinc-950/70 shadow-[0_0_0_1px_rgba(24,24,27,0.2)]', className)}
  {...rest}
>
  {@render children()}
</div>
