<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let {
    ref = $bindable(null),
    orientation = 'vertical',
    class: className,
    children,
    ...restProps
  }: ScrollAreaPrimitive.ScrollbarProps = $props();
</script>

<ScrollAreaPrimitive.Scrollbar
  bind:ref
  data-slot="scroll-area-scrollbar"
  {orientation}
  class={cn(
    'flex touch-none select-none p-px transition-colors',
    orientation === 'vertical'
      ? 'h-full w-2.5 border-l border-l-transparent'
      : 'h-2.5 flex-col border-t border-t-transparent',
    className
  )}
  {...restProps}
>
  {@render children?.()}
</ScrollAreaPrimitive.Scrollbar>
