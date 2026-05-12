<script lang="ts">
  import { onMount } from 'svelte';
  import { Trash2, Wrench } from 'lucide-svelte';

  import { WorkspaceEmptyState, WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Textarea } from '$lib/components/ui/textarea';
  import { deleteSkill, fetchSkills, upsertSkill } from '$lib/nucleus/client';
  import type { SkillManifest } from '$lib/nucleus/schemas';

  let skills: SkillManifest[] = [];
  let loading = true;
  let saving = false;
  let error: string | null = null;
  let success: string | null = null;

  let form: SkillManifest = {
    id: '',
    title: '',
    description: '',
    instructions: '',
    activation_mode: 'manual',
    triggers: [],
    include_paths: [],
    required_tools: [],
    required_mcps: [],
    project_filters: [],
    enabled: true
  };

  function resetForm(skill?: SkillManifest) {
    form = skill
      ? {
          ...skill,
          triggers: [...skill.triggers],
          include_paths: [...skill.include_paths],
          required_tools: [...skill.required_tools],
          required_mcps: [...skill.required_mcps],
          project_filters: [...skill.project_filters],
          instructions: skill.instructions || ''
        }
      : {
          id: '',
          title: '',
          description: '',
          instructions: '',
          activation_mode: 'manual',
          triggers: [],
          include_paths: [],
          required_tools: [],
          required_mcps: [],
          project_filters: [],
          enabled: true
        };
  }

  function parseList(value: string) {
    return value
      .split(/\n|,/)
      .map((item) => item.trim())
      .filter(Boolean);
  }

  async function load() {
    loading = true;
    try {
      skills = await fetchSkills();
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load skills.';
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    success = null;
    error = null;
    try {
      await upsertSkill(form);
      success = `Saved skill ${form.id}.`;
      resetForm();
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save skill.';
    } finally {
      saving = false;
    }
  }

  async function removeSkill(id: string) {
    if (!confirm(`Delete skill ${id}?`)) return;
    try {
      await deleteSkill(id);
      if (form.id === id) resetForm();
      success = `Deleted skill ${id}.`;
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete skill.';
    }
  }

  onMount(load);
</script>

<svelte:head>
  <title>Nucleus - Skills</title>
</svelte:head>

<div class="space-y-8">
  <WorkspacePageHeader
    title="Skills"
    description="Manage daemon-backed workspace Skills that contribute prompt context and dependency hints."
  />

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">{error}</div>
  {/if}
  {#if success}
    <div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">{success}</div>
  {/if}

  <div class="grid gap-6 2xl:grid-cols-[minmax(360px,0.8fr)_minmax(560px,1.2fr)]">
    <Card>
      <CardHeader>
        <CardTitle>Existing Skills</CardTitle>
        <CardDescription>Select a Skill to edit its metadata and markdown instructions.</CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        {#if loading}
          <div class="text-sm text-zinc-400">Loading skills…</div>
        {:else if skills.length === 0}
          <WorkspaceEmptyState message="No Skills configured yet." />
        {:else}
          {#each skills as skill}
            <div class="rounded-lg border border-zinc-800 bg-zinc-950/40 p-4">
              <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div class="min-w-0 space-y-2">
                  <div class="flex flex-wrap items-center gap-2">
                    <div class="font-medium text-zinc-50">{skill.title}</div>
                    <Badge variant={skill.enabled ? 'default' : 'secondary'}>{skill.enabled ? 'Enabled' : 'Disabled'}</Badge>
                    <Badge variant="secondary">{skill.activation_mode}</Badge>
                  </div>
                  <div class="text-xs text-zinc-500">{skill.id}</div>
                  <p class="line-clamp-3 text-sm text-zinc-300">{skill.description || 'No description yet.'}</p>
                  <div class="grid gap-2 text-xs text-zinc-400 sm:grid-cols-2">
                    <div><span class="text-zinc-500">Triggers:</span> {skill.triggers.join(', ') || '—'}</div>
                    <div><span class="text-zinc-500">Include paths:</span> {skill.include_paths.join(', ') || '—'}</div>
                    <div><span class="text-zinc-500">Required MCPs:</span> {skill.required_mcps.join(', ') || '—'}</div>
                    <div><span class="text-zinc-500">Project filters:</span> {skill.project_filters.join(', ') || '—'}</div>
                  </div>
                </div>
                <div class="flex gap-2">
                  <Button variant="secondary" onclick={() => resetForm(skill)}>Edit</Button>
                  <Button variant="destructive" onclick={() => removeSkill(skill.id)}>
                    <Trash2 class="size-4" />
                  </Button>
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-center gap-2"><Wrench class="size-5" /> Skill Editor</CardTitle>
        <CardDescription>Edit the manifest and the Skill markdown instructions that Nucleus injects into prompts.</CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <div class="grid gap-4 lg:grid-cols-2">
          <div class="space-y-1"><Label for="skill-id">ID</Label><Input id="skill-id" bind:value={form.id} /></div>
          <div class="space-y-1"><Label for="skill-title">Title</Label><Input id="skill-title" bind:value={form.title} /></div>
        </div>
        <div class="space-y-1"><Label for="skill-description">Description</Label><Textarea id="skill-description" bind:value={form.description} rows={3} /></div>
        <div class="space-y-1"><Label for="skill-instructions">SKILL.md instructions</Label><Textarea id="skill-instructions" bind:value={form.instructions} rows={18} class="font-mono text-xs leading-relaxed" placeholder="# Skill name&#10;&#10;Write the instructions this skill should contribute to prompt context." /></div>
        <div class="space-y-1"><Label for="skill-activation">Activation mode</Label><select id="skill-activation" class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100" bind:value={form.activation_mode}><option value="manual">manual</option><option value="auto">auto</option><option value="always">always</option></select></div>
        <div class="space-y-1"><Label for="skill-triggers">Triggers</Label><Textarea id="skill-triggers" value={form.triggers.join('\n')} oninput={(event) => (form.triggers = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div>
        <div class="space-y-1"><Label for="skill-includes">Include paths</Label><Textarea id="skill-includes" value={form.include_paths.join('\n')} oninput={(event) => (form.include_paths = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div>
        <div class="space-y-1"><Label for="skill-mcps">Required MCPs</Label><Textarea id="skill-mcps" value={form.required_mcps.join('\n')} oninput={(event) => (form.required_mcps = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={2} /></div>
        <div class="space-y-1"><Label for="skill-projects">Project filters</Label><Textarea id="skill-projects" value={form.project_filters.join('\n')} oninput={(event) => (form.project_filters = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={2} /></div>
        <label class="flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>
        <div class="flex gap-2">
          <Button onclick={save} disabled={saving || !form.id || !form.title}>{saving ? 'Saving…' : 'Save skill'}</Button>
          <Button variant="secondary" onclick={() => resetForm()}>Reset</Button>
        </div>
      </CardContent>
    </Card>
  </div>
</div>
