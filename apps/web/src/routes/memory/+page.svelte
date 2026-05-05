<script lang="ts">
  import { onMount } from 'svelte';
  import { MemoryStick, RefreshCw, ShieldCheck } from 'lucide-svelte';

  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import ProcessTable from '$lib/components/dashboard/process-table.svelte';
  import { fetchProcesses, fetchSystemStats, killProcess } from '$lib/nucleus/client';
  import { clampPercent, formatBytes, formatClock, formatCount, formatPercent } from '$lib/nucleus/format';
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

  async function loadAll(silent = false) {
    if (!silent) {
      loading = system === null;
    }

    refreshing = silent;

    try {
      const [nextSystem, nextProcesses] = await Promise.all([
        fetchSystemStats(),
        fetchProcesses({ sort: 'memory', limit: 30 })
      ]);

      system = nextSystem;
      processData = nextProcesses;
      updatedAt = Date.now();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read memory telemetry.';
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
        if (event.data.sort === 'memory') {
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
  <title>Nucleus - Memory</title>
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
        <h1 class="text-3xl font-semibold text-zinc-50">Memory</h1>
        <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
          Live memory pressure, available headroom, and the heaviest user-owned processes on this host.
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

  <ProcessTable
    title="Top memory processes"
    subtitle={processData
      ? `Showing ${formatCount(processData.processes.length)} of ${formatCount(processData.meta.matching_processes)} processes for ${processData.meta.current_user}.`
      : 'Waiting for daemon process data.'}
    processes={processData?.processes ?? []}
    sort="memory"
    {killingPid}
    {killConfirmPid}
    onKill={handleKill}
  />
</div>
