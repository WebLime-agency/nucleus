<script lang="ts">
  import { cn } from '$lib/utils';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLDivElement> {
    orientation?: 'horizontal' | 'vertical';
  }

  let { orientation = 'horizontal', class: className, ...rest }: Props = $props();
</script>

<div
  role="separator"
  aria-orientation={orientation}
  class={cn(
    'shrink-0 bg-zinc-800',
    orientation === 'vertical' ? 'h-full w-px' : 'h-px w-full',
    className
  )}
  {...rest}
></div>
