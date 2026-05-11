<script lang="ts">
  import { cn } from '$lib/utils';

  let {
    title,
    detail,
    value,
    tone = 'lime',
    footer,
    icon,
    class: className
  }: {
    title: string;
    detail?: string;
    value: number;
    tone?: 'lime' | 'cyan';
    footer?: import('svelte').Snippet;
    icon?: import('svelte').Snippet;
    class?: string;
  } = $props();

  const barClass = $derived(
    tone === 'cyan' ? 'bg-cyan-300/80' : 'bg-lime-300/80'
  );
  const clamped = $derived(Math.max(0, Math.min(100, value)));
</script>

<div class={cn('rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4', className)}>
  <div class="mb-3 flex items-center justify-between gap-3">
    <div class="inline-flex items-center gap-2 text-zinc-200">
      {#if icon}
        {@render icon()}
      {/if}
      {title}
    </div>
    {#if detail}
      <div class="font-mono text-xs text-zinc-400">{detail}</div>
    {/if}
  </div>
  <div class="h-3 rounded-full bg-zinc-900">
    <div class={cn('h-3 rounded-full transition-all', barClass)} style={`width: ${clamped}%`}></div>
  </div>
  {#if footer}
    <div class="mt-3 flex items-center justify-between gap-3 text-xs text-zinc-500">
      {@render footer()}
    </div>
  {/if}
</div>
