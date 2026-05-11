<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { ref = $bindable(null), class: className, ...restProps }: ScrollAreaPrimitive.CornerProps = $props();
</script>

<ScrollAreaPrimitive.Corner
  bind:ref
  data-slot="scroll-area-corner"
  class={cn('bg-transparent', className)}
  {...restProps}
/>
