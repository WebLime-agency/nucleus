<script lang="ts">
  import { onMount } from 'svelte';
  import { Cpu, RefreshCw } from 'lucide-svelte';

  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import ProcessTable from '$lib/components/dashboard/process-table.svelte';
  import { fetchProcesses, fetchSystemStats, killProcess } from '$lib/nucleus/client';
  import { clampPercent, formatClock, formatCount, formatPercent } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, ProcessListResponse, SystemStats } from '$lib/nucleus/schemas';

  let system = $state<SystemStats | null>(null);
  let processData = $state<ProcessListResponse | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let updatedAt = $state<number | null>(null);
  let killingPid = $state<number | null>(null);
  let killConfirmPid = $state<number | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

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

  async function loadAll(silent = false) {
    if (!silent) {
      loading = system === null;
    }

    refreshing = silent;

    try {
      const [nextSystem, nextProcesses] = await Promise.all([
        fetchSystemStats(),
        fetchProcesses({ sort: 'cpu', limit: 30 })
      ]);

      system = nextSystem;
      processData = nextProcesses;
      updatedAt = Date.now();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read CPU telemetry.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function refreshNow() {
    killConfirmPid = null;
    await loadAll(true);
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
        if (event.data.sort === 'cpu') {
          processData = event.data.response;
          updatedAt = Date.now();
          error = null;
        }
        break;
      case 'overview.updated':
        break;
    }

    loading = false;
    refreshing = false;
  }

  onMount(() => {
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
  <title>Nucleus - CPU</title>
</svelte:head>

<div class="space-y-8">
  <section class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
    <div class="space-y-3">
      <Badge variant={error ? 'destructive' : 'default'}>
        {#if loading}
          Connecting
        {:else if error}
          Degraded
        {:else if refreshing}
          Refreshing
        {:else if streamStatus === 'reconnecting'}
          Reconnecting
        {:else if streamStatus === 'connecting'}
          Connecting
        {:else if updatedAt}
          Updated {formatClock(updatedAt)}
        {:else}
          Waiting
        {/if}
      </Badge>
      <div>
        <h1 class="text-3xl font-semibold text-zinc-50">CPU</h1>
        <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
          Per-core CPU telemetry and the busiest user-owned processes, all sourced from the Rust daemon.
        </p>
      </div>
    </div>

    <Button variant="outline" onclick={refreshNow} disabled={refreshing}>
      <RefreshCw class={refreshing ? 'size-4 animate-spin' : 'size-4'} />
      {refreshing ? 'Refreshing' : 'Refresh'}
    </Button>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

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

  <ProcessTable
    title="Top CPU processes"
    subtitle={processData
      ? `Showing ${formatCount(processData.processes.length)} of ${formatCount(processData.meta.matching_processes)} processes for ${processData.meta.current_user}.`
      : 'Waiting for daemon process data.'}
    processes={processData?.processes ?? []}
    sort="cpu"
    {killingPid}
    {killConfirmPid}
    onKill={handleKill}
  />
</div>
