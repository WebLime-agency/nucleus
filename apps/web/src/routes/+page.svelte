<script lang="ts">
  import { onMount } from 'svelte';
  import { Bot, FolderTree, RefreshCw, Server, Workflow } from 'lucide-svelte';

  import ProcessTable from '$lib/components/dashboard/process-table.svelte';
  import DiskTable from '$lib/components/dashboard/disk-table.svelte';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { fetchOverview, fetchProcesses, fetchSystemStats, killProcess } from '$lib/nucleus/client';
  import { formatBytes, formatClock, formatCount, formatPercent, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, ProcessListResponse, RuntimeOverview, SystemStats } from '$lib/nucleus/schemas';

  let overview = $state<RuntimeOverview | null>(null);
  let system = $state<SystemStats | null>(null);
  let processData = $state<ProcessListResponse | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let updatedAt = $state<number | null>(null);
  let killingPid = $state<number | null>(null);
  let killConfirmPid = $state<number | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (error) return 'Degraded';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (updatedAt === null) return 'Waiting';
    return `Updated ${formatClock(updatedAt)}`;
  });

  async function loadAll(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      const [nextOverview, nextSystem, nextProcesses] = await Promise.all([
        fetchOverview(),
        fetchSystemStats(),
        fetchProcesses({ sort: 'memory', limit: 12 })
      ]);

      overview = nextOverview;
      system = nextSystem;
      processData = nextProcesses;
      updatedAt = Date.now();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to reach the daemon.';
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
      case 'overview.updated':
        overview = event.data;
        updatedAt = Date.now();
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
  <title>Nucleus - Overview</title>
  <meta
    name="description"
    content="Nucleus host operations overview with daemon-backed system telemetry, storage paths, and process controls."
  />
</svelte:head>

<div class="space-y-8">
  <section class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
    <div class="space-y-3">
      <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
      <div>
        <h1 class="text-3xl font-semibold text-zinc-50">Overview</h1>
        <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
          Rust owns the system data and process actions. This web client is only reading and steering
          the daemon contract.
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
        <CardDescription>CPU load</CardDescription>
        <CardTitle>{system ? formatPercent(system.cpu.load_percent) : '--'}</CardTitle>
      </CardHeader>
      <CardContent class="text-sm text-zinc-400">
        {#if system}
          {system.cpu.cores.length} logical cores on {system.hostname}
        {:else}
          Waiting for daemon metrics.
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardDescription>Memory usage</CardDescription>
        <CardTitle>{system ? formatPercent(system.memory.used_percent) : '--'}</CardTitle>
      </CardHeader>
      <CardContent class="text-sm text-zinc-400">
        {#if system}
          {formatBytes(system.memory.used_bytes)} of {formatBytes(system.memory.total_bytes)}
        {:else}
          Waiting for daemon metrics.
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardDescription>Processes</CardDescription>
        <CardTitle>{system ? formatCount(system.process_count) : '--'}</CardTitle>
      </CardHeader>
      <CardContent class="text-sm text-zinc-400">
        {#if processData}
          {formatCount(processData.meta.matching_processes)} owned by {processData.meta.current_user}
        {:else}
          Waiting for daemon metrics.
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardDescription>Managed runtimes</CardDescription>
        <CardTitle>{overview ? formatCount(overview.runtimes.length) : '--'}</CardTitle>
      </CardHeader>
      <CardContent class="text-sm text-zinc-400">
        {#if overview}
          {formatCount(overview.sessions.length)} tracked sessions
        {:else}
          Waiting for daemon state.
        {/if}
      </CardContent>
    </Card>
  </section>

  <section class="grid gap-4 xl:grid-cols-[1.2fr_0.8fr]">
    <DiskTable disks={system?.disks ?? []} />

    <div class="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Runtimes</CardTitle>
          <CardDescription>Daemon-probed provider readiness and adapter inventory.</CardDescription>
        </CardHeader>
        <CardContent class="space-y-3">
          {#if overview && overview.runtimes.length > 0}
            {#each overview.runtimes as runtime}
              <div class="rounded-md border border-zinc-800 bg-zinc-950/50 px-4 py-3">
                <div class="flex items-center justify-between gap-3">
                  <div class="flex items-center gap-2">
                    <Server class="size-4 text-zinc-500" />
                    <span class="font-medium text-zinc-100">{runtime.id}</span>
                  </div>
                  <Badge variant="secondary">{formatState(runtime.state)}</Badge>
                </div>
                <p class="mt-2 text-sm text-zinc-400">{runtime.summary}</p>
                <div class="mt-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                  <span>Auth {formatState(runtime.auth_state)}</span>
                  {#if runtime.default_model}
                    <span>{runtime.default_model}</span>
                  {/if}
                </div>
                {#if runtime.note}
                  <p class="mt-2 text-xs leading-5 text-zinc-500">{runtime.note}</p>
                {/if}
              </div>
            {/each}
          {:else}
            <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
              No runtimes available yet.
            </div>
          {/if}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Storage</CardTitle>
          <CardDescription>Current local persistence roots under the Nucleus state directory.</CardDescription>
        </CardHeader>
        <CardContent class="space-y-3">
          {#if overview}
            <div class="rounded-md border border-zinc-800 bg-zinc-950/50 px-4 py-3">
              <div class="flex items-center gap-2 text-zinc-200">
                <FolderTree class="size-4 text-zinc-500" />
                State root
              </div>
              <div class="mt-2 break-all font-mono text-xs text-zinc-400">{overview.storage.state_dir}</div>
            </div>
            <div class="rounded-md border border-zinc-800 bg-zinc-950/50 px-4 py-3">
              <div class="flex items-center gap-2 text-zinc-200">
                <Workflow class="size-4 text-zinc-500" />
                Database
              </div>
              <div class="mt-2 break-all font-mono text-xs text-zinc-400">{overview.storage.database_path}</div>
            </div>
            <div class="rounded-md border border-zinc-800 bg-zinc-950/50 px-4 py-3">
              <div class="flex items-center gap-2 text-zinc-200">
                <Bot class="size-4 text-zinc-500" />
                Memory
              </div>
              <div class="mt-2 break-all font-mono text-xs text-zinc-400">{overview.storage.memory_dir}</div>
            </div>
          {:else}
            <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
              Waiting for storage metadata.
            </div>
          {/if}
        </CardContent>
      </Card>
    </div>
  </section>

  <ProcessTable
    title="Top processes"
    subtitle={processData
      ? `Showing ${formatCount(processData.processes.length)} of ${formatCount(processData.meta.matching_processes)} user-owned processes.`
      : 'Waiting for daemon process data.'}
    processes={processData?.processes ?? []}
    sort="memory"
    {killingPid}
    {killConfirmPid}
    onKill={handleKill}
  />
</div>
