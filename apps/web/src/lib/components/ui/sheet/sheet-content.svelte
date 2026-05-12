<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';

  interface Props extends DialogPrimitive.ContentProps {
    children: Snippet;
    side?: 'left' | 'right';
    portalDisabled?: boolean;
    overlayClass?: string;
  }

  let { ref = $bindable(null), class: className, children, side = 'right', portalDisabled = false, overlayClass, ...restProps }: Props = $props();
</script>

<DialogPrimitive.Portal disabled={portalDisabled}>
  <DialogPrimitive.Overlay class={cn('fixed inset-0 z-40 bg-black/50 md:bg-black/20', overlayClass)} />
  <DialogPrimitive.Content
    bind:ref
    class={cn(
      'fixed inset-y-0 z-50 flex w-full flex-col border-zinc-800 bg-zinc-950 shadow-2xl outline-none transition-all md:w-[min(720px,92vw)]',
      side === 'right' ? 'right-0 border-l' : 'left-0 border-r',
      className
    )}
    {...restProps}
  >
    {@render children()}
  </DialogPrimitive.Content>
</DialogPrimitive.Portal>
