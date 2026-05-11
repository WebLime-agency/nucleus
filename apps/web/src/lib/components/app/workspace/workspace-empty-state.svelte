<script lang="ts">
  import { cn } from '$lib/utils';

  let {
    message,
    class: className
  }: {
    message: string;
    class?: string;
  } = $props();
</script>

<div class={cn('rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500', className)}>
  {message}
</div>
