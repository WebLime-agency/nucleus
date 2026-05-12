<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';
  let { ref = $bindable(null), class: className, ...restProps }: DialogPrimitive.OverlayProps = $props();
</script>

<DialogPrimitive.Overlay bind:ref class={cn('fixed inset-0 z-50 bg-black/60', className)} {...restProps} />
