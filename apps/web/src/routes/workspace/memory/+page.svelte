<script lang="ts">
  import { onMount } from 'svelte';
  import { BookOpenText, Brain, Database, FileClock } from 'lucide-svelte';

  import { WorkspaceEmptyState, WorkspacePageHeader, WorkspaceStoragePathCard } from '$lib/components/app/workspace';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { fetchOverview } from '$lib/nucleus/client';
  import { compactPath } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type { DaemonEvent, RuntimeOverview } from '$lib/nucleus/schemas';

  let overview = $state<RuntimeOverview | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let storage = $derived(overview?.storage ?? null);

  async function loadAll() {
    try {
      overview = await fetchOverview();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load memory state.';
    } finally {
      loading = false;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event !== 'overview.updated') {
      return;
    }

    overview = event.data;
    loading = false;
    error = null;
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
  <WorkspacePageHeader
    title="Memory"
    description="This surface is reserved for long-term memory, reusable knowledge, and operator-managed context. Host RAM moved into Diagnostics so the product language stays clean."
  />

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  <section class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2">
          <Brain class="size-5 text-zinc-400" />
          Memory Layer
        </CardTitle>
        <CardDescription>
          Profile-scoped and workspace-scoped memory editing lands here next.
        </CardDescription>
      </CardHeader>
      <CardContent class="text-sm leading-6 text-zinc-400">
        Nucleus already owns the storage roots. The next pass is exposing user-editable memory
        entries the way Agent0 surfaces durable context.
      </CardContent>
    </Card>

    <WorkspaceStoragePathCard
      title="Stored State"
      description="Nucleus keeps memory artifacts and structured state under its local storage root."
      paths={[
        storage ? compactPath(storage.memory_dir) : 'Waiting for storage details',
        storage ? compactPath(storage.transcripts_dir) : 'Waiting for transcript details'
      ]}
    />

    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2">
          <BookOpenText class="size-5 text-zinc-400" />
          Prompt Includes
        </CardTitle>
        <CardDescription>
          Long-term memory will complement, not replace, the include and workspace knowledge model.
        </CardDescription>
      </CardHeader>
      <CardContent class="text-sm leading-6 text-zinc-400">
        Include directories still shape prompt-time context. This page is for durable memory that an
        operator explicitly wants Nucleus to preserve, edit, and apply over time.
      </CardContent>
    </Card>
  </section>

  <Card>
    <CardHeader>
      <CardTitle class="flex items-center gap-2">
        <FileClock class="size-5 text-zinc-400" />
        Next Phase
      </CardTitle>
      <CardDescription>
        The UI layer is ready for real memory controls once the Nucleus memory model is finalized.
      </CardDescription>
    </CardHeader>
    <CardContent class="text-sm leading-6 text-zinc-400">
      Expect profile-aware memory entries, workspace-level notes, import and pruning controls, and a
      clear distinction between prompt includes, transcripts, and durable memory records.
    </CardContent>
  </Card>
</div>
