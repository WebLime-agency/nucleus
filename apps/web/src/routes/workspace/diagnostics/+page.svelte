<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { Cpu, MemoryStick, ShieldCheck } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import ProcessTable from '$lib/components/dashboard/process-table.svelte';
  import { fetchProcesses, fetchSystemStats, killProcess } from '$lib/nucleus/client';
  import {
    clampPercent,
    formatBytes,
    formatClock,
    formatCount,
    formatPercent
  } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, ProcessListResponse, SystemStats } from '$lib/nucleus/schemas';

  type DiagnosticsView = 'cpu' | 'memory';

  let view = $state<DiagnosticsView>('cpu');
  let system = $state<SystemStats | null>(null);
  let processCache = $state<Record<DiagnosticsView, ProcessListResponse | null>>({
    cpu: null,
    memory: null
  });
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let updatedAt = $state<number | null>(null);
  let killingPid = $state<number | null>(null);
  let killConfirmPid = $state<number | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let processData = $derived(processCache[view]);
  let hottestCore = $derived.by(() => {
    if (!system || system.cpu.cores.length === 0) return 0;
    return Math.max(...system.cpu.cores.map((core) => core.usage_percent));
  });
  let averageFrequency = $derived.by(() => {
    if (!system || system.cpu.cores.length === 0) return 0;
    return Math.round(
      system.cpu.cores.reduce((sum, core) => sum + core.frequency_mhz, 0) / system.cpu.cores.length
    );
  });
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (error) return 'Degraded';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (updatedAt) return `Updated ${formatClock(updatedAt)}`;
    return 'Waiting';
  });

  function syncRequestedView() {
    const requested = page.url.searchParams.get('view');
    view = requested === 'memory' ? 'memory' : 'cpu';
  }

  async function loadAll(silent = false) {
    if (!silent) {
      loading = system === null;
    }

    refreshing = silent;

    try {
      const [nextSystem, cpuProcesses, memoryProcesses] = await Promise.all([
        fetchSystemStats(),
        fetchProcesses({ sort: 'cpu', limit: 30 }),
        fetchProcesses({ sort: 'memory', limit: 30 })
      ]);

      system = nextSystem;
      processCache = {
        cpu: cpuProcesses,
        memory: memoryProcesses
      };
      updatedAt = Date.now();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read host diagnostics.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function refreshNow() {
    killConfirmPid = null;
    await loadAll(true);
  }

  async function switchView(next: DiagnosticsView) {
    if (next === view) {
      return;
    }

    await goto(`/workspace/diagnostics?view=${next}`, {
      noScroll: true,
      keepFocus: true,
      replaceState: true
    });
  }

  async function handleKill(pid: number) {
    if (killConfirmPid !== pid) {
      killConfirmPid = pid;
      return;
    }

    killingPid = pid;
    killConfirmPid = null;

    try {
      await killProcess(pid);
      await loadAll(true);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to stop the process.';
    } finally {
      killingPid = null;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    switch (event.event) {
      case 'connected':
        error = null;
        break;
      case 'system.updated':
        system = event.data;
        updatedAt = Date.now();
        error = null;
        break;
      case 'processes.updated':
        processCache = {
          ...processCache,
          [event.data.sort]: event.data.response
        };
        updatedAt = Date.now();
        error = null;
        break;
      case 'overview.updated':
        break;
    }

    loading = false;
    refreshing = false;
  }

  $effect(() => {
    syncRequestedView();
  });

  onMount(() => {
    syncRequestedView();
    void loadAll();

    const disconnect = connectDaemonStream({
      onEvent: applyStreamEvent,
      onStatusChange: (status) => {
        streamStatus = status;
      },
      onError: (message) => {
        error = message;
      }
    });

    return () => {
      disconnect();
    };
  });
</script>

<svelte:head>
  <title>Nucleus - Diagnostics</title>
</svelte:head>

<div class="space-y-8">
  <section class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
    <div>
      <div class="flex flex-wrap items-center gap-3">
        <h1 class="text-3xl font-semibold text-zinc-50">Diagnostics</h1>
        <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
      </div>
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
        Host CPU and RAM now live together so the memory surface can shift toward long-term agent
        memory instead of machine telemetry.
      </p>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <div class="inline-flex rounded-lg border border-zinc-800 bg-zinc-950/70 p-1">
        <button
          type="button"
          class={`inline-flex h-9 items-center gap-2 rounded-md px-3 text-sm transition-colors ${
            view === 'cpu'
              ? 'bg-zinc-100 text-zinc-950'
              : 'text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
          }`}
          onclick={() => switchView('cpu')}
        >
          <Cpu class="size-4" />
          CPU
        </button>
        <button
          type="button"
          class={`inline-flex h-9 items-center gap-2 rounded-md px-3 text-sm transition-colors ${
            view === 'memory'
              ? 'bg-zinc-100 text-zinc-950'
              : 'text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
          }`}
          onclick={() => switchView('memory')}
        >
          <MemoryStick class="size-4" />
          RAM
        </button>
      </div>
    </div>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  {#if view === 'cpu'}
    <section class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatPercent(system.cpu.load_percent) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Overall CPU load</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatCount(system.cpu.cores.length) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Logical cores</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{formatPercent(hottestCore)}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Busiest core</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{averageFrequency > 0 ? `${formatCount(averageFrequency)} MHz` : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Average frequency</CardContent>
      </Card>
    </section>

    <Card>
      <CardHeader>
        <CardTitle>Per-core activity</CardTitle>
      </CardHeader>
      <CardContent>
        {#if system}
          <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
            {#each system.cpu.cores as core}
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
        {:else}
          <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
            Waiting for CPU telemetry.
          </div>
        {/if}
      </CardContent>
    </Card>
  {:else}
    <section class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatBytes(system.memory.used_bytes) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Used memory</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatBytes(system.memory.available_bytes) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Available memory</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatBytes(system.memory.free_bytes) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Free memory</CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{system ? formatPercent(system.memory.used_percent) : '--'}</CardTitle>
        </CardHeader>
        <CardContent class="text-sm text-zinc-400">Overall memory pressure</CardContent>
      </Card>
    </section>

    <Card>
      <CardHeader>
        <CardTitle>Memory pressure</CardTitle>
      </CardHeader>
      <CardContent>
        {#if system}
          <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4">
            <div class="mb-3 flex items-center justify-between gap-3">
              <div class="inline-flex items-center gap-2 text-zinc-200">
                <MemoryStick class="size-4 text-zinc-500" />
                In use
              </div>
              <div class="font-mono text-xs text-zinc-400">
                {formatBytes(system.memory.used_bytes)} / {formatBytes(system.memory.total_bytes)}
              </div>
            </div>
            <div class="h-3 rounded-full bg-zinc-900">
              <div
                class="h-3 rounded-full bg-cyan-300/80 transition-all"
                style={`width: ${clampPercent(system.memory.used_percent)}%`}
              ></div>
            </div>
            <div class="mt-3 flex items-center justify-between gap-3 text-xs text-zinc-500">
              <span>{formatBytes(system.memory.available_bytes)} available</span>
              <span class="inline-flex items-center gap-1">
                <ShieldCheck class="size-3.5 text-lime-300/80" />
                {formatCount(system.process_count)} total processes
              </span>
            </div>
          </div>
        {:else}
          <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
            Waiting for memory telemetry.
          </div>
        {/if}
      </CardContent>
    </Card>
  {/if}

  <ProcessTable
    title={view === 'cpu' ? 'Top CPU processes' : 'Top memory processes'}
    subtitle={processData
      ? `Showing ${formatCount(processData.processes.length)} of ${formatCount(processData.meta.matching_processes)} processes for ${processData.meta.current_user}.`
      : 'Waiting for Nucleus process data.'}
    processes={processData?.processes ?? []}
    sort={view}
    {killingPid}
    {killConfirmPid}
    onKill={handleKill}
  />
</div>
