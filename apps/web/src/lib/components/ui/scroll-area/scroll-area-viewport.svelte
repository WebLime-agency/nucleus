<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { ref = $bindable(null), class: className, children, ...restProps }: ScrollAreaPrimitive.ViewportProps = $props();
</script>

<ScrollAreaPrimitive.Viewport
  bind:ref
  data-slot="scroll-area-viewport"
  class={cn('size-full rounded-[inherit]', className)}
  {...restProps}
>
  {@render children?.()}
</ScrollAreaPrimitive.Viewport>
