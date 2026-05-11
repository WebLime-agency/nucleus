<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { ref = $bindable(null), class: className, children, ...restProps }: ScrollAreaPrimitive.RootProps = $props();
</script>

<ScrollAreaPrimitive.Root
  bind:ref
  data-slot="scroll-area"
  class={cn('relative overflow-hidden', className)}
  {...restProps}
>
  {@render children?.()}
</ScrollAreaPrimitive.Root>
