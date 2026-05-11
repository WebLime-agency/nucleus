<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { ref = $bindable(null), class: className, ...restProps }: ScrollAreaPrimitive.ThumbProps = $props();
</script>

<ScrollAreaPrimitive.Thumb
  bind:ref
  data-slot="scroll-area-thumb"
  class={cn('relative flex-1 rounded-full bg-zinc-700/80 hover:bg-zinc-600', className)}
  {...restProps}
/>
