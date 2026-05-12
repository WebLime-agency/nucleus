<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { Cpu, MemoryStick, ShieldCheck } from 'lucide-svelte';

  import {
    WorkspaceCoreGrid,
    WorkspaceEmptyState,
    WorkspaceMeterPanel,
    WorkspacePageHeader,
    WorkspaceSegmentedControl,
    WorkspaceStatCard
  } from '$lib/components/app/workspace';
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
  <WorkspacePageHeader
    title="Diagnostics"
    description="Host CPU and RAM now live together so the memory surface can shift toward long-term agent memory instead of machine telemetry."
    badge={statusLabel}
    badgeVariant={error ? 'destructive' : 'default'}
  >
    {#snippet actions()}
      <WorkspaceSegmentedControl
        items={[
          { value: 'cpu', label: 'CPU', icon: Cpu },
          { value: 'memory', label: 'RAM', icon: MemoryStick }
        ]}
        value={view}
        onChange={(next) => switchView(next as DiagnosticsView)}
      />

      <Button variant="outline" onclick={refreshNow} disabled={refreshing}>
        {refreshing ? 'Refreshing…' : 'Refresh'}
      </Button>
    {/snippet}
  </WorkspacePageHeader>

  {#if view === 'cpu'}
    <section class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <WorkspaceStatCard
        value={system ? formatPercent(system.cpu.load_percent) : '--'}
        label="Overall CPU load"
      />
      <WorkspaceStatCard
        value={system ? formatCount(system.cpu.cores.length) : '--'}
        label="Logical cores"
      />
      <WorkspaceStatCard value={formatPercent(hottestCore)} label="Busiest core" />
      <WorkspaceStatCard
        value={averageFrequency > 0 ? `${formatCount(averageFrequency)} MHz` : '--'}
        label="Average frequency"
      />
    </section>

    <Card>
      <CardHeader>
        <CardTitle>Per-core activity</CardTitle>
      </CardHeader>
      <CardContent>
        {#if system}
          <WorkspaceCoreGrid cores={system.cpu.cores} {clampPercent} />
        {:else}
          <WorkspaceEmptyState message="Waiting for CPU telemetry." />
        {/if}
      </CardContent>
    </Card>
  {:else}
    <section class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <WorkspaceStatCard
        value={system ? formatBytes(system.memory.used_bytes) : '--'}
        label="Used memory"
      />
      <WorkspaceStatCard
        value={system ? formatBytes(system.memory.available_bytes) : '--'}
        label="Available memory"
      />
      <WorkspaceStatCard
        value={system ? formatBytes(system.memory.free_bytes) : '--'}
        label="Free memory"
      />
      <WorkspaceStatCard
        value={system ? formatPercent(system.memory.used_percent) : '--'}
        label="Overall memory pressure"
      />
    </section>

    <Card>
      <CardHeader>
        <CardTitle>Memory pressure</CardTitle>
      </CardHeader>
      <CardContent>
        {#if system}
          <WorkspaceMeterPanel
            title="In use"
            detail={`${formatBytes(system.memory.used_bytes)} / ${formatBytes(system.memory.total_bytes)}`}
            value={clampPercent(system.memory.used_percent)}
            tone="cyan"
          >
            {#snippet icon()}
              <MemoryStick class="size-4 text-zinc-500" />
            {/snippet}
            {#snippet footer()}
              <span>{formatBytes(system!.memory.available_bytes)} available</span>
              <span class="inline-flex items-center gap-1">
                <ShieldCheck class="size-3.5 text-lime-300/80" />
                {formatCount(system!.process_count)} total processes
              </span>
            {/snippet}
          </WorkspaceMeterPanel>
        {:else}
          <WorkspaceEmptyState message="Waiting for memory telemetry." />
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
