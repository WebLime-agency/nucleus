<script lang="ts">
  import { onMount } from 'svelte';
  import { AlertTriangle, Brain, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-svelte';

  import { WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { fetchMemory, upsertMemory, deleteMemory } from '$lib/nucleus/client';
  import type { MemoryEntry, MemoryEntryUpsertRequest } from '$lib/nucleus/schemas';

  let entries = $state<MemoryEntry[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let editingId = $state<string | null>(null);
  let form = $state({
    title: '',
    content: '',
    scope_kind: 'workspace',
    scope_id: 'workspace',
    memory_kind: 'note',
    status: 'accepted',
    tags: '',
    enabled: true
  });

  let acceptedEntries = $derived(entries.filter((entry) => entry.status === 'accepted'));

  async function loadMemory() {
    loading = true;
    try {
      const summary = await fetchMemory();
      entries = summary.entries;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load memory entries.';
    } finally {
      loading = false;
    }
  }

  function resetForm() {
    editingId = null;
    form = { title: '', content: '', scope_kind: 'workspace', scope_id: 'workspace', memory_kind: 'note', status: 'accepted', tags: '', enabled: true };
  }

  function editEntry(entry: MemoryEntry) {
    editingId = entry.id;
    form = {
      title: entry.title,
      content: entry.content,
      scope_kind: entry.scope_kind,
      scope_id: entry.scope_id,
      memory_kind: entry.memory_kind,
      status: entry.status,
      tags: entry.tags.join(', '),
      enabled: entry.enabled
    };
  }

  async function saveEntry() {
    saving = true;
    try {
      const payload: MemoryEntryUpsertRequest = {
        id: editingId ?? undefined,
        title: form.title,
        content: form.content,
        scope_kind: form.scope_kind,
        scope_id: form.scope_id,
        memory_kind: form.memory_kind,
        status: form.status,
        source_kind: 'manual',
        created_by: 'user',
        tags: form.tags.split(',').map((tag) => tag.trim()).filter(Boolean),
        enabled: form.enabled
      };
      await upsertMemory(payload);
      await loadMemory();
      resetForm();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save memory entry.';
    } finally {
      saving = false;
    }
  }

  async function setEnabled(entry: MemoryEntry, enabled: boolean) {
    await upsertMemory({ ...entry, enabled });
    await loadMemory();
  }

  async function archiveEntry(entry: MemoryEntry) {
    await upsertMemory({ ...entry, status: 'archived' });
    await loadMemory();
  }

  async function removeEntry(entry: MemoryEntry) {
    await deleteMemory(entry.id);
    await loadMemory();
  }

  function formatTime(value?: number | null) {
    if (!value) return 'Never';
    return new Date(value * 1000).toLocaleString();
  }

  onMount(() => {
    void loadMemory();
  });
</script>

<svelte:head><title>Nucleus - Memory</title></svelte:head>

<div class="space-y-6">
  <WorkspacePageHeader title="Memory" description="Manage accepted durable context that Nucleus can include in future compiled prompts." />

  {#if error}<div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">{error}</div>{/if}

  <Card class="border-amber-500/30 bg-amber-500/10">
    <CardContent class="flex gap-3 pt-6 text-sm text-amber-100">
      <AlertTriangle class="mt-0.5 size-5 shrink-0" />
      <div><strong>Memory is prompt-visible context.</strong> Do not store secrets, API keys, passwords, private keys, cookies, or bearer tokens here. Use Vault for credentials and secret material.</div>
    </CardContent>
  </Card>

  <section class="grid gap-4 lg:grid-cols-[minmax(0,1fr)_24rem]">
    <div class="space-y-4">
      <div class="flex items-center justify-between">
        <h2 class="text-lg font-semibold text-zinc-100">Accepted memory ({acceptedEntries.length})</h2>
        <Button variant="outline" size="sm" onclick={() => void loadMemory()}><RefreshCw class="mr-2 size-4" />Refresh</Button>
      </div>

      {#if loading}
        <Card><CardContent class="py-8 text-sm text-zinc-400">Loading memory entries…</CardContent></Card>
      {:else if entries.length === 0}
        <Card><CardContent class="py-8 text-sm text-zinc-400">No memory entries yet. Create manual accepted memory to make it available to prompt compilation.</CardContent></Card>
      {:else}
        {#each entries as entry (entry.id)}
          <Card class={entry.status !== 'accepted' || !entry.enabled ? 'opacity-70' : ''}>
            <CardHeader>
              <div class="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <CardTitle class="flex items-center gap-2"><Brain class="size-4 text-zinc-400" />{entry.title}</CardTitle>
                  <CardDescription>{entry.scope_kind}/{entry.scope_id} · {entry.memory_kind} · {entry.status} · {entry.source_kind}{entry.source_id ? `/${entry.source_id}` : ''}</CardDescription>
                </div>
                <div class="flex flex-wrap gap-2">
                  <Button variant="outline" size="sm" onclick={() => editEntry(entry)}><Pencil class="mr-2 size-4" />Edit</Button>
                  <Button variant="outline" size="sm" onclick={() => void setEnabled(entry, !entry.enabled)}>{entry.enabled ? 'Disable' : 'Enable'}</Button>
                  <Button variant="outline" size="sm" onclick={() => void archiveEntry(entry)}>Archive</Button>
                  <Button variant="destructive" size="sm" onclick={() => void removeEntry(entry)}><Trash2 class="mr-2 size-4" />Delete</Button>
                </div>
              </div>
            </CardHeader>
            <CardContent class="space-y-3 text-sm text-zinc-300">
              <p class="whitespace-pre-wrap leading-6">{entry.content}</p>
              <div class="flex flex-wrap gap-2 text-xs text-zinc-500">
                <span>Tags: {entry.tags.length ? entry.tags.join(', ') : 'none'}</span>
                <span>Created: {formatTime(entry.created_at)}</span>
                <span>Updated: {formatTime(entry.updated_at)}</span>
                <span>Last used: {formatTime(entry.last_used_at)}</span>
                <span>Use count: {entry.use_count}</span>
              </div>
            </CardContent>
          </Card>
        {/each}
      {/if}
    </div>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2"><Plus class="size-5 text-zinc-400" />{editingId ? 'Edit memory' : 'Create memory'}</CardTitle>
        <CardDescription>Manual entries default to accepted and can be disabled or archived later.</CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        <input class="w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" placeholder="Title" bind:value={form.title} />
        <textarea class="min-h-36 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" placeholder="Prompt-visible memory content" bind:value={form.content}></textarea>
        <div class="grid grid-cols-2 gap-2">
          <select class="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" bind:value={form.scope_kind}><option value="workspace">workspace</option><option value="project">project</option><option value="session">session</option></select>
          <input class="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" placeholder="Scope id" bind:value={form.scope_id} />
          <select class="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" bind:value={form.memory_kind}><option value="note">note</option><option value="preference">preference</option><option value="instruction">instruction</option><option value="fact">fact</option></select>
          <select class="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" bind:value={form.status}><option value="accepted">accepted</option><option value="archived">archived</option></select>
        </div>
        <input class="w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm" placeholder="Tags, comma separated" bind:value={form.tags} />
        <label class="flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>
        <div class="flex gap-2">
          <Button disabled={saving || !form.title || !form.content || !form.scope_id} onclick={() => void saveEntry()}>{saving ? 'Saving…' : 'Save memory'}</Button>
          {#if editingId}<Button variant="outline" onclick={resetForm}>Cancel</Button>{/if}
        </div>
      </CardContent>
    </Card>
  </section>
</div>
