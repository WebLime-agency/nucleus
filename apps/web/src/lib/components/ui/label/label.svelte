<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLLabelAttributes } from 'svelte/elements';

  interface Props extends HTMLLabelAttributes {
    children: Snippet;
  }

  let { class: className, children, ...rest }: Props = $props();
</script>

<label
  class={cn('text-xs font-medium uppercase tracking-[0.14em] text-zinc-500', className)}
  {...rest}
>
  {@render children()}
</label>
