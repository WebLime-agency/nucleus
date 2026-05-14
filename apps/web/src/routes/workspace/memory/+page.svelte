<script lang="ts">
  import { onMount } from 'svelte';
  import { AlertTriangle, Brain, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-svelte';

  import { WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Checkbox } from '$lib/components/ui/checkbox';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
  import { Textarea } from '$lib/components/ui/textarea';
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
    try {
      await upsertMemory({ ...entry, enabled });
      await loadMemory();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update memory entry.';
    }
  }

  async function archiveEntry(entry: MemoryEntry) {
    try {
      await upsertMemory({ ...entry, status: 'archived' });
      await loadMemory();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to archive memory entry.';
    }
  }

  async function removeEntry(entry: MemoryEntry) {
    try {
      await deleteMemory(entry.id);
      await loadMemory();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete memory entry.';
    }
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
                  <CardDescription class="mt-2 flex flex-wrap gap-2">
                    <Badge variant="outline">{entry.scope_kind}/{entry.scope_id}</Badge>
                    <Badge variant="secondary">{entry.memory_kind}</Badge>
                    <Badge variant={entry.status === 'accepted' ? 'default' : 'secondary'}>{entry.status}</Badge>
                    <Badge variant="outline">{entry.source_kind}{entry.source_id ? `/${entry.source_id}` : ''}</Badge>
                  </CardDescription>
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
        <div class="space-y-2">
          <Label for="memory-title">Title</Label>
          <Input id="memory-title" placeholder="Title" bind:value={form.title} />
        </div>
        <div class="space-y-2">
          <Label for="memory-content">Content</Label>
          <Textarea id="memory-content" class="min-h-36" placeholder="Prompt-visible memory content" bind:value={form.content} />
        </div>
        <div class="grid grid-cols-2 gap-2">
          <div class="space-y-2">
            <Label for="memory-scope-kind">Scope</Label>
            <Select id="memory-scope-kind" bind:value={form.scope_kind}><option value="workspace">workspace</option><option value="project">project</option><option value="session">session</option></Select>
          </div>
          <div class="space-y-2">
            <Label for="memory-scope-id">Scope id</Label>
            <Input id="memory-scope-id" placeholder="Scope id" bind:value={form.scope_id} />
          </div>
          <div class="space-y-2">
            <Label for="memory-kind">Kind</Label>
            <Select id="memory-kind" bind:value={form.memory_kind}><option value="note">note</option><option value="fact">fact</option><option value="preference">preference</option><option value="decision">decision</option><option value="project_note">project_note</option><option value="solution">solution</option><option value="constraint">constraint</option><option value="todo">todo</option></Select>
          </div>
          <div class="space-y-2">
            <Label for="memory-status">Status</Label>
            <Select id="memory-status" bind:value={form.status}><option value="accepted">accepted</option><option value="archived">archived</option></Select>
          </div>
        </div>
        <div class="space-y-2">
          <Label for="memory-tags">Tags</Label>
          <Input id="memory-tags" placeholder="Tags, comma separated" bind:value={form.tags} />
        </div>
        <Label class="flex items-center gap-2 text-sm text-zinc-300"><Checkbox bind:checked={form.enabled} /> Enabled</Label>
        <div class="flex gap-2">
          <Button disabled={saving || !form.title || !form.content || !form.scope_id} onclick={() => void saveEntry()}>{saving ? 'Saving…' : 'Save memory'}</Button>
          {#if editingId}<Button variant="outline" onclick={resetForm}>Cancel</Button>{/if}
        </div>
      </CardContent>
    </Card>
  </section>
</div>
