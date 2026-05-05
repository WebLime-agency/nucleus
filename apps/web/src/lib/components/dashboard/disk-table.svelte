<script lang="ts">
  import { HardDrive } from 'lucide-svelte';

  import type { DiskStat } from '$lib/nucleus/schemas';
  import { clampPercent, formatBytes, formatPercent } from '$lib/nucleus/format';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';

  let {
    disks = []
  }: {
    disks?: DiskStat[];
  } = $props();
</script>

<Card>
  <CardHeader>
    <CardTitle>Disk Usage</CardTitle>
    <CardDescription>Active local mounts surfaced by the Rust daemon.</CardDescription>
  </CardHeader>
  <CardContent>
    {#if disks.length === 0}
      <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
        No disks matched the current host filter.
      </div>
    {:else}
      <div class="max-w-full overflow-x-auto rounded-md border border-zinc-800">
        <table class="w-full min-w-[760px] text-sm">
          <thead class="bg-zinc-950/90">
            <tr class="border-b border-zinc-800">
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Mount</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Disk</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">File System</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Used</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Available</th>
              <th class="px-4 py-3 text-right text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Total</th>
              <th class="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-[0.18em] text-zinc-500">Usage</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-zinc-800">
            {#each disks as disk}
              {@const usage = disk.total_bytes === 0 ? 0 : (disk.used_bytes / disk.total_bytes) * 100}
              <tr class="bg-zinc-950/40">
                <td class="px-4 py-3 font-mono text-xs text-zinc-300">{disk.mount_point}</td>
                <td class="px-4 py-3 text-zinc-200">
                  <div class="flex items-center gap-2">
                    <HardDrive class="size-4 text-zinc-500" />
                    <span>{disk.name || 'Disk'}</span>
                  </div>
                </td>
                <td class="px-4 py-3 text-zinc-400">{disk.file_system}</td>
                <td class="px-4 py-3 text-right text-zinc-200">{formatBytes(disk.used_bytes)}</td>
                <td class="px-4 py-3 text-right text-zinc-200">{formatBytes(disk.available_bytes)}</td>
                <td class="px-4 py-3 text-right text-zinc-200">{formatBytes(disk.total_bytes)}</td>
                <td class="px-4 py-3">
                  <div class="flex items-center gap-3">
                    <div class="h-2 flex-1 rounded-full bg-zinc-900">
                      <div
                        class="h-2 rounded-full bg-lime-300/80 transition-all"
                        style={`width: ${clampPercent(usage)}%`}
                      ></div>
                    </div>
                    <span class="w-14 text-right font-mono text-xs text-zinc-400">
                      {formatPercent(usage)}
                    </span>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </CardContent>
</Card>
