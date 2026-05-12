<script lang="ts">
  import { Cpu } from 'lucide-svelte';
  import { formatCount, formatPercent } from '$lib/nucleus/format';

  type CoreStat = {
    id: number;
    usage_percent: number;
    frequency_mhz: number;
  };

  let {
    cores,
    clampPercent
  }: {
    cores: CoreStat[];
    clampPercent: (value: number) => number;
  } = $props();
</script>

<div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
  {#each cores as core}
    <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
      <div class="mb-2 flex items-center justify-between gap-3">
        <div class="inline-flex items-center gap-2 text-sm text-zinc-200">
          <Cpu class="size-4 text-zinc-500" />
          Core {core.id}
        </div>
        <span class="font-mono text-xs text-zinc-400">{formatPercent(core.usage_percent)}</span>
      </div>
      <div class="h-2 rounded-full bg-zinc-900">
        <div
          class="h-2 rounded-full bg-lime-300/80 transition-all"
          style={`width: ${clampPercent(core.usage_percent)}%`}
        ></div>
      </div>
      <div class="mt-2 text-xs text-zinc-500">{formatCount(core.frequency_mhz)} MHz</div>
    </div>
  {/each}
</div>
