<script lang="ts">
  import { onMount } from 'svelte';
  import { AlertTriangle, Bug, Circle, Info, RefreshCcw } from 'lucide-svelte';

  import {
    WorkspaceEmptyState,
    WorkspacePageHeader,
    WorkspaceSegmentedControl,
    WorkspaceStatCard
  } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { fetchInstanceLogs } from '$lib/nucleus/client';
  import { compactPath, formatCount, formatDateTime, formatState } from '$lib/nucleus/format';
  import type { InstanceLogEntry, InstanceLogListResponse } from '$lib/nucleus/schemas';

  type SeverityFilter = 'all' | 'error' | 'warn' | 'info' | 'debug';

  const severityItems = [
    { value: 'all', label: 'All', icon: Circle },
    { value: 'error', label: 'Errors', icon: AlertTriangle },
    { value: 'warn', label: 'Warnings', icon: AlertTriangle },
    { value: 'info', label: 'Info', icon: Info },
    { value: 'debug', label: 'Debug', icon: Bug }
  ];

  let response = $state<InstanceLogListResponse | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let category = $state('all');
  let severity = $state<SeverityFilter>('all');

  let records = $derived(response?.records ?? []);
  let categories = $derived(response?.categories ?? []);
  let logsDir = $derived(response?.logs_dir ?? '');
  let retention = $derived(response?.retention ?? '');
  let activeCategoryCount = $derived(
    category === 'all'
      ? records.length
      : categories.find((item) => item.category === category)?.count ?? 0
  );
  let statusLabel = $derived.by(() => {
    if (loading) return 'Loading';
    if (error) return 'Degraded';
    if (refreshing) return 'Refreshing';
    return `${formatCount(records.length)} recent`;
  });

  async function loadLogs(silent = false) {
    if (!silent) {
      loading = response === null;
    }

    refreshing = silent;

    try {
      response = await fetchInstanceLogs({
        category: category === 'all' ? undefined : category,
        level: severity === 'all' ? undefined : severity,
        limit: 150
      });
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read instance logs.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  function selectCategory(next: string) {
    category = next;
    void loadLogs(true);
  }

  function selectSeverity(next: string) {
    severity = next as SeverityFilter;
    void loadLogs(true);
  }

  function levelVariant(level: string): 'default' | 'secondary' | 'outline' | 'warning' | 'destructive' {
    if (level === 'error') return 'destructive';
    if (level === 'warn') return 'warning';
    if (level === 'debug') return 'secondary';
    return 'default';
  }

  function renderJson(value: unknown) {
    if (!value || (typeof value === 'object' && Object.keys(value).length === 0)) {
      return '';
    }
    return JSON.stringify(value);
  }

  function trackById(record: InstanceLogEntry) {
    return record.id;
  }

  onMount(() => {
    void loadLogs();
  });
</script>

<svelte:head>
  <title>Nucleus - Logs</title>
</svelte:head>

<div class="space-y-8">
  <WorkspacePageHeader
    title="Logs"
    description="Instance-local product events for support and debugging. These records are daemon-owned, redacted before persistence, and kept out of prompt context."
    badge={statusLabel}
    badgeVariant={error ? 'destructive' : 'default'}
  >
    {#snippet actions()}
      <Button variant="outline" onclick={() => loadLogs(true)} disabled={refreshing}>
        <RefreshCcw class="size-4" />
        {refreshing ? 'Refreshing' : 'Refresh'}
      </Button>
    {/snippet}
  </WorkspacePageHeader>

  <section class="grid gap-4 md:grid-cols-3">
    <WorkspaceStatCard value={formatCount(records.length)} label="Visible records" />
    <WorkspaceStatCard value={formatCount(activeCategoryCount)} label="Active category count" />
    <WorkspaceStatCard value={formatCount(categories.length)} label="Categories" />
  </section>

  <section class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_18rem]">
    <Card>
      <CardHeader class="gap-4">
        <div class="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
          <CardTitle>Recent events</CardTitle>
          <div class="max-w-full overflow-x-auto pb-1">
            <WorkspaceSegmentedControl
              items={severityItems}
              value={severity}
              onChange={selectSeverity}
            />
          </div>
        </div>
        <div class="flex max-w-full flex-wrap gap-2">
          <Button
            variant={category === 'all' ? 'default' : 'outline'}
            size="sm"
            onclick={() => selectCategory('all')}
          >
            All
          </Button>
          {#each categories as item (item.category)}
            <Button
              variant={category === item.category ? 'default' : 'outline'}
              size="sm"
              onclick={() => selectCategory(item.category)}
            >
              {formatState(item.category)}
            </Button>
          {/each}
        </div>
      </CardHeader>
      <CardContent>
        {#if error}
          <WorkspaceEmptyState message={error} />
        {:else if loading}
          <WorkspaceEmptyState message="Loading instance logs." />
        {:else if records.length === 0}
          <WorkspaceEmptyState message="No log records match the current filters." />
        {:else}
          <div class="overflow-x-auto rounded-md border border-zinc-800">
            <div class="grid min-w-[44rem] grid-cols-[9rem_6rem_7rem_minmax(0,1fr)] border-b border-zinc-800 bg-zinc-950 px-3 py-2 text-xs font-medium uppercase text-zinc-500">
              <span>Time</span>
              <span>Level</span>
              <span>Category</span>
              <span>Event</span>
            </div>
            <div class="divide-y divide-zinc-900">
              {#each records as record (trackById(record))}
                <article class="grid min-w-[44rem] grid-cols-[9rem_6rem_7rem_minmax(0,1fr)] gap-2 px-3 py-3 text-sm">
                  <time class="text-zinc-500" datetime={new Date(record.timestamp * 1000).toISOString()}>
                    {formatDateTime(record.timestamp)}
                  </time>
                  <div>
                    <Badge variant={levelVariant(record.level)}>{formatState(record.level)}</Badge>
                  </div>
                  <div class="text-zinc-300">{formatState(record.category)}</div>
                  <div class="min-w-0">
                    <div class="font-medium text-zinc-100">{record.event}</div>
                    <p class="mt-1 text-zinc-400">{record.message}</p>
                    {#if renderJson(record.related_ids)}
                      <p class="mt-2 break-all font-mono text-xs text-zinc-500">
                        {renderJson(record.related_ids)}
                      </p>
                    {/if}
                  </div>
                </article>
              {/each}
            </div>
          </div>
        {/if}
      </CardContent>
    </Card>

    <aside class="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Filesystem</CardTitle>
        </CardHeader>
        <CardContent class="space-y-3 text-sm text-zinc-400">
          <p class="break-all font-mono text-xs text-zinc-300">
            {logsDir ? compactPath(logsDir) : 'Waiting for daemon path.'}
          </p>
          <p>Structured JSONL is written to events.jsonl in this directory.</p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Retention</CardTitle>
        </CardHeader>
        <CardContent class="text-sm leading-6 text-zinc-400">
          {retention || 'Waiting for daemon retention settings.'}
        </CardContent>
      </Card>
    </aside>
  </section>
</div>
