<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLHeadingElement> {
    children: Snippet;
  }

  let { class: className, children, ...rest }: Props = $props();
</script>

<h3 class={cn('text-sm font-semibold text-zinc-100', className)} {...rest}>
  {@render children()}
</h3>
