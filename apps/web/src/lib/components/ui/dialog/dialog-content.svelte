<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';

  interface Props extends DialogPrimitive.ContentProps {
    children: Snippet;
    portalDisabled?: boolean;
  }

  let { ref = $bindable(null), class: className, children, portalDisabled = false, ...restProps }: Props = $props();
</script>

<DialogPrimitive.Portal disabled={portalDisabled}>
  <DialogPrimitive.Overlay class="fixed inset-0 z-50 bg-black/60" />
  <DialogPrimitive.Content
    bind:ref
    class={cn('fixed left-1/2 top-1/2 z-50 w-[calc(100vw-2rem)] max-w-2xl -translate-x-1/2 -translate-y-1/2 rounded-lg border border-zinc-800 bg-zinc-950 p-5 shadow-2xl outline-none', className)}
    {...restProps}
  >
    {@render children()}
  </DialogPrimitive.Content>
</DialogPrimitive.Portal>
