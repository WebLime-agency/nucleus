<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLDivElement> {
    children: Snippet;
  }

  let { class: className, children, ...rest }: Props = $props();
</script>

<div class={cn('flex flex-col gap-1 px-5 py-4', className)} {...rest}>
  {@render children()}
</div>
