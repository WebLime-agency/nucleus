<script lang="ts">
  import { Activity, Power, SquareTerminal } from 'lucide-svelte';

  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import type { ProcessSnapshot } from '$lib/nucleus/schemas';
  import { clampPercent, compactPath, formatBytes, formatPercent } from '$lib/nucleus/format';

  type SortMode = 'cpu' | 'memory';

  let {
    title,
    subtitle = '',
    processes = [],
    sort = 'memory',
    killingPid = null,
    killConfirmPid = null,
    onKill
  }: {
    title: string;
    subtitle?: string;
    processes?: ProcessSnapshot[];
    sort?: SortMode;
    killingPid?: number | null;
    killConfirmPid?: number | null;
    onKill: (pid: number) => void | Promise<void>;
  } = $props();

  let maxValue = $derived.by(() => {
    if (processes.length === 0) return 1;

    return sort === 'cpu'
      ? Math.max(...processes.map((process) => process.cpu_percent), 1)
      : Math.max(...processes.map((process) => process.memory_bytes), 1);
  });

  function metricLabel(process: ProcessSnapshot): string {
    return sort === 'cpu'
      ? formatPercent(process.cpu_percent)
      : formatBytes(process.memory_bytes);
  }

  function metricWidth(process: ProcessSnapshot): number {
    const raw = sort === 'cpu'
      ? (process.cpu_percent / maxValue) * 100
      : (process.memory_bytes / maxValue) * 100;

    return clampPercent(raw);
  }
</script>

<Card>
  <CardHeader>
    <CardTitle>{title}</CardTitle>
    <CardDescription>{subtitle}</CardDescription>
  </CardHeader>
  <CardContent>
    {#if processes.length === 0}
      <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
        No processes available for this view.
      </div>
    {:else}
      <div class="max-w-full overflow-x-auto rounded-md border border-zinc-800">
        <table class="w-full min-w-[980px] text-sm">
          <thead class="bg-zinc-950/90">
            <tr class="border-b border-zinc-800">
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Name</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Project</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">PID</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Primary</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Memory</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">CPU</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Command</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">State</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Action</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-zinc-800">
            {#each processes as process}
              <tr class={killConfirmPid === process.pid ? 'bg-red-500/5' : 'bg-zinc-950/40'}>
                <td class="px-4 py-3">
                  <div class="font-medium text-zinc-100">{process.name}</div>
                  <div class="mt-1 text-xs text-zinc-500">{process.user}</div>
                </td>
                <td class="px-4 py-3 font-mono text-xs text-zinc-400">
                  {#if process.cwd}
                    <span title={process.cwd}>{compactPath(process.cwd)}</span>
                  {:else}
                    -
                  {/if}
                </td>
                <td class="px-4 py-3 text-right font-mono text-xs text-zinc-300">{process.pid}</td>
                <td class="px-4 py-3">
                  <div class="flex items-center gap-3">
                    <div class="h-2 flex-1 rounded-full bg-zinc-900">
                      <div
                        class={sort === 'cpu' ? 'h-2 rounded-full bg-lime-300/80 transition-all' : 'h-2 rounded-full bg-cyan-300/80 transition-all'}
                        style={`width: ${metricWidth(process)}%`}
                      ></div>
                    </div>
                    <span class="w-20 text-right font-mono text-xs text-zinc-300">
                      {metricLabel(process)}
                    </span>
                  </div>
                </td>
                <td class="px-4 py-3 text-right font-mono text-xs text-zinc-300">
                  {formatBytes(process.memory_bytes)}
                </td>
                <td class="px-4 py-3 text-right font-mono text-xs text-zinc-300">
                  {formatPercent(process.cpu_percent)}
                </td>
                <td class="px-4 py-3">
                  <div class="flex items-start gap-2 text-zinc-400">
                    <SquareTerminal class="mt-0.5 size-4 shrink-0 text-zinc-600" />
                    <div>
                      <div class="font-mono text-xs text-zinc-300">{process.command}</div>
                      {#if process.params}
                        <div class="mt-1 max-w-[360px] truncate text-xs text-zinc-500" title={process.params}>
                          {process.params}
                        </div>
                      {/if}
                    </div>
                  </div>
                </td>
                <td class="px-4 py-3">
                  <div class="inline-flex items-center gap-2 rounded-full bg-zinc-900 px-2.5 py-1 text-[11px] text-zinc-300">
                    <Activity class="size-3.5 text-lime-300/80" />
                    {process.status}
                  </div>
                </td>
                <td class="px-4 py-3 text-right">
                  <Button
                    size="sm"
                    variant={killConfirmPid === process.pid ? 'destructive' : 'ghost'}
                    onclick={() => onKill(process.pid)}
                    disabled={killingPid === process.pid}
                  >
                    <Power class="size-3.5" />
                    {#if killingPid === process.pid}
                      Stopping
                    {:else if killConfirmPid === process.pid}
                      Confirm
                    {:else}
                      Stop
                    {/if}
                  </Button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </CardContent>
</Card>
