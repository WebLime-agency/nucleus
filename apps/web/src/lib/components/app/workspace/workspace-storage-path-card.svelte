<script lang="ts">
  import { compactPath } from '$lib/nucleus/format';

  let {
    label,
    path,
    empty = 'Unavailable',
    title,
    description,
    paths
  }: {
    label?: string;
    path?: string | null | undefined;
    empty?: string;
    title?: string;
    description?: string;
    paths?: string[];
  } = $props();
</script>

{#if title}
  <div class="rounded-xl border border-zinc-800 bg-zinc-950/40">
    <div class="border-b border-zinc-800 px-5 py-4">
      <div class="text-sm font-medium text-zinc-100">{title}</div>
      {#if description}
        <div class="mt-1 text-sm text-zinc-500">{description}</div>
      {/if}
    </div>
    <div class="space-y-2 px-5 py-4">
      {#each paths ?? [] as item}
        <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3 text-sm text-zinc-100">
          {item}
        </div>
      {/each}
    </div>
  </div>
{:else}
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">{label}</div>
    <div class="mt-2 text-sm text-zinc-100" title={path ?? undefined}>
      {path ? compactPath(path) : empty}
    </div>
  </div>
{/if}
