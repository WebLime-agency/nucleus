<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { CheckCircle2, ChevronsRightLeft, Download, Maximize2, Minimize2, RefreshCw, Search, Trash2, Wrench, X } from 'lucide-svelte';

  import { WorkspaceEmptyState, WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Dialog, DialogContent, DialogDescription, DialogTitle } from '$lib/components/ui/dialog';
  import { Select } from '$lib/components/ui/select';
  import { Separator } from '$lib/components/ui/separator';
  import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from '$lib/components/ui/sheet';
  import { Textarea } from '$lib/components/ui/textarea';
  import {
    checkSkillUpdate,
    checkSkillUpdates,
    deleteSkill,
    fetchSkillInstallations,
    fetchSkillPackages,
    fetchSkills,
    importSkills,
    reconcileSkills,
    scanReconcileSkills,
    upsertSkill
  } from '$lib/nucleus/client';
  import type { SkillImportResponse, SkillInstallationRecord, SkillManifest, SkillPackageRecord, SkillReconcileScanResponse } from '$lib/nucleus/schemas';

  let skills: SkillManifest[] = [];
  let packages: SkillPackageRecord[] = [];
  let installations: SkillInstallationRecord[] = [];
  let loading = true;
  let saving = false;
  let pendingAction: string | null = null;
  let error: string | null = null;
  let success: string | null = null;
  let result: SkillImportResponse | null = null;

  let selectedSkillId: string | null = null;
  let drawerOpen = false;
  let drawerExpanded = false;
  let importOpen = false;
  let reconcileOpen = false;
  let reconcileScan: SkillReconcileScanResponse | null = null;
  let selectedReconcileSkillIds: string[] = [];
  let reconcileCandidateFilter = 'unregistered';
  let reconcileCandidateSearch = '';
  let search = '';
  let enabledFilter = 'all';
  let sourceFilter = 'all';
  let updateFilter = 'all';
  let importSource = '';
  let importScopeKind = 'workspace';
  let importScopeId = 'default';

  let form: SkillManifest = blankSkill();

  $: selectedPackage = packageForSkill(form.id || selectedSkillId || '');
  $: selectedInstallation = installationForPackage(selectedPackage?.id || '');
  $: filteredSkills = skills.filter(matchesFilters);
  $: filteredReconcileCandidates = reconcileScan
    ? reconcileScan.candidates.filter((candidate) => {
        const query = reconcileCandidateSearch.trim().toLowerCase();
        const matchesQuery = !query || [candidate.title, candidate.skill_id, candidate.path].join(' ').toLowerCase().includes(query);
        const matchesFilter =
          reconcileCandidateFilter === 'all' ||
          (reconcileCandidateFilter === 'registered' && candidate.already_registered) ||
          (reconcileCandidateFilter === 'unregistered' && !candidate.already_registered);
        return matchesQuery && matchesFilter;
      })
    : [];

  function blankSkill(): SkillManifest {
    return { id: '', title: '', description: '', instructions: '', activation_mode: 'manual', triggers: [], include_paths: [], required_tools: [], required_mcps: [], project_filters: [], enabled: true };
  }

  function resetForm(skill?: SkillManifest | null) {
    form = skill
      ? { ...skill, triggers: [...skill.triggers], include_paths: [...skill.include_paths], required_tools: [...skill.required_tools], required_mcps: [...skill.required_mcps], project_filters: [...skill.project_filters], instructions: skill.instructions || '' }
      : blankSkill();
  }

  function selectSkill(skill: SkillManifest) {
    selectedSkillId = skill.id;
    resetForm(skill);
    drawerOpen = true;
    tick().then(() => document.getElementById('skill-drawer-title')?.focus());
  }

  function closeDrawer() {
    drawerOpen = false;
    drawerExpanded = false;
  }

  function parseList(value: string) {
    return value.split(/\n|,/).map((item) => item.trim()).filter(Boolean);
  }

  function packageForSkill(skillId: string) {
    return packages.find((pkg) => packageSkillId(pkg) === skillId || pkg.name === skillId || pkg.id === skillId) ?? null;
  }

  function packageSkillId(pkg: SkillPackageRecord) {
    const manifest = pkg.manifest_json as Record<string, unknown> | null;
    return typeof manifest?.id === 'string' ? manifest.id : '';
  }

  function installationForPackage(packageId: string) {
    return installations.find((installation) => installation.package_id === packageId) ?? null;
  }

  function matchesFilters(skill: SkillManifest) {
    const pkg = packageForSkill(skill.id);
    const query = search.trim().toLowerCase();
    const haystack = [skill.title, skill.id, skill.description, pkg?.source_kind, pkg?.source_url, pkg?.source_repo, pkg?.source_ref, pkg?.source_skill_path].filter(Boolean).join(' ').toLowerCase();
    if (query && !haystack.includes(query)) return false;
    if (enabledFilter !== 'all' && String(skill.enabled) !== enabledFilter) return false;
    if (sourceFilter !== 'all' && (pkg?.source_kind || 'unknown') !== sourceFilter) return false;
    if (updateFilter !== 'all' && (pkg?.update_status || 'unknown') !== updateFilter) return false;
    return true;
  }

  function badgeVariant(value: string) {
    return value === 'current' || value === 'clean' || value === 'installed' ? 'default' : value === 'update_available' || value === 'modified' ? 'outline' : value === 'source_error' ? 'destructive' : 'secondary';
  }

  function sourceSummary(pkg: SkillPackageRecord | null) {
    if (!pkg) return 'No package registration found.';
    if (pkg.source_repo || pkg.source_ref || pkg.source_skill_path) return [pkg.source_repo, pkg.source_ref, pkg.source_skill_path].filter(Boolean).join(' · ');
    return pkg.source_url || pkg.source_repo_url || 'No source details recorded.';
  }

  function formatTime(value?: number | null) {
    if (!value) return '—';
    return new Date(value * 1000).toLocaleString();
  }

  async function load() {
    loading = true;
    try {
      [skills, packages, installations] = await Promise.all([fetchSkills(), fetchSkillPackages(), fetchSkillInstallations()]);
      if (selectedSkillId && !skills.some((skill) => skill.id === selectedSkillId)) closeDrawer();
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
      const saved = await upsertSkill(form);
      success = `Saved skill ${saved.id}.`;
      selectedSkillId = saved.id;
      await load();
      resetForm(skills.find((skill) => skill.id === saved.id) ?? saved);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save skill.';
    } finally {
      saving = false;
    }
  }

  async function removeSkill(id: string) {
    if (!confirm(`Delete skill ${id}?`)) return;
    pendingAction = `delete:${id}`;
    try {
      await deleteSkill(id);
      if (selectedSkillId === id) closeDrawer();
      success = `Deleted skill ${id}.`;
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete skill.';
    } finally {
      pendingAction = null;
    }
  }

  async function runImport() {
    pendingAction = 'import';
    error = null;
    result = null;
    try {
      result = await importSkills({ source: importSource, scope_kind: importScopeKind, scope_id: importScopeId });
      success = `Import completed with ${result.installed.length} installed skill${result.installed.length === 1 ? '' : 's'}.`;
      await load();
      if (result.installed[0]) {
        selectedSkillId = result.installed[0].skill_id;
        resetForm(skills.find((skill) => skill.id === selectedSkillId));
        drawerOpen = true;
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to import skills.';
    } finally {
      pendingAction = null;
    }
  }

  async function openReconcileDialog() {
    reconcileOpen = true;
    pendingAction = 'reconcile-scan';
    error = null;
    result = null;
    reconcileScan = null;
    selectedReconcileSkillIds = [];
    reconcileCandidateFilter = 'unregistered';
    reconcileCandidateSearch = '';
    try {
      reconcileScan = await scanReconcileSkills();
      selectedReconcileSkillIds = reconcileScan.candidates
        .filter((candidate) => !candidate.already_registered)
        .map((candidate) => candidate.skill_id);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to scan local skill folders.';
      reconcileOpen = false;
    } finally {
      pendingAction = null;
    }
  }

  function toggleReconcileSkill(skillId: string, checked: boolean) {
    selectedReconcileSkillIds = checked
      ? Array.from(new Set([...selectedReconcileSkillIds, skillId]))
      : selectedReconcileSkillIds.filter((id) => id !== skillId);
  }

  function selectVisibleReconcileSkills() {
    selectedReconcileSkillIds = Array.from(
      new Set([
        ...selectedReconcileSkillIds,
        ...filteredReconcileCandidates
          .filter((candidate) => !candidate.already_registered)
          .map((candidate) => candidate.skill_id)
      ])
    );
  }

  function clearVisibleReconcileSkills() {
    const visible = new Set(filteredReconcileCandidates.map((candidate) => candidate.skill_id));
    selectedReconcileSkillIds = selectedReconcileSkillIds.filter((skillId) => !visible.has(skillId));
  }

  async function runReconcile() {
    pendingAction = 'reconcile';
    result = null;
    try {
      result = await reconcileSkills({ skill_ids: selectedReconcileSkillIds });
      success = `Registered ${result.installed.length} local skill${result.installed.length === 1 ? '' : 's'} from the scan.`;
      reconcileOpen = false;
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to register selected local skills.';
    } finally {
      pendingAction = null;
    }
  }

  async function runCheckUpdates(skillId?: string) {
    pendingAction = skillId ? `check:${skillId}` : 'check-all';
    result = null;
    try {
      if (skillId) {
        const checked = await checkSkillUpdate(skillId);
        result = { installed: [checked], errors: [] };
        success = `${skillId}: ${checked.update_status}.`;
      } else {
        const checked = await checkSkillUpdates();
        result = checked;
        success = `Checked ${checked.installed.length} skill${checked.installed.length === 1 ? '' : 's'} for updates.`;
      }
      await load();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to check skill updates.';
    } finally {
      pendingAction = null;
    }
  }

  onMount(load);
</script>

<svelte:head><title>Nucleus - Skills</title></svelte:head>

<div class="space-y-6">
  <WorkspacePageHeader title="Skills" description="Import, inspect, and edit daemon-backed workspace Skills." />

  {#if error}<div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">{error}</div>{/if}
  {#if success}<div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">{success}</div>{/if}

  <Card>
    <CardHeader class="gap-4 lg:flex-row lg:items-start lg:justify-between">
      <div>
        <CardTitle>Skill library</CardTitle>
        <CardDescription>Import copies skill files into Nucleus, registers the package, and installs it. Source-backed skills can be checked for updates.</CardDescription>
      </div>
      <div class="flex flex-wrap gap-2">
        <Button variant="secondary" onclick={() => (importOpen = true)}><Download class="size-4" /> Import</Button>
        <Button variant="outline" onclick={openReconcileDialog} disabled={pendingAction === 'reconcile-scan'} title="Scan the Nucleus state skills directory before choosing which folders to register."><ChevronsRightLeft class="size-4" /> {pendingAction === 'reconcile-scan' ? 'Scanning…' : 'Scan local skills'}</Button>
        <Button variant="outline" onclick={() => runCheckUpdates()} disabled={pendingAction === 'check-all'}><RefreshCw class="size-4" /> {pendingAction === 'check-all' ? 'Checking…' : 'Check all updates'}</Button>
      </div>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid gap-3 lg:grid-cols-[minmax(220px,1fr)_150px_150px_170px]">
        <div class="relative"><Search class="pointer-events-none absolute left-3 top-3 size-4 text-zinc-500" /><Input class="pl-9" placeholder="Search name, id, description, source…" bind:value={search} /></div>
        <Select bind:value={enabledFilter} aria-label="Enabled filter"><option value="all">All enabled</option><option value="true">Enabled</option><option value="false">Disabled</option></Select>
        <Select bind:value={sourceFilter} aria-label="Source filter"><option value="all">All sources</option><option value="github">GitHub</option><option value="local">Local</option><option value="manual">Manual</option><option value="unknown">Unknown</option></Select>
        <Select bind:value={updateFilter} aria-label="Update filter"><option value="all">All updates</option><option value="current">Current</option><option value="update_available">Update available</option><option value="source_error">Source error</option><option value="unknown">Unknown</option></Select>
      </div>

      {#if loading}
        <div class="text-sm text-zinc-400">Loading skills…</div>
      {:else if filteredSkills.length === 0}
        <WorkspaceEmptyState message={skills.length === 0 ? 'No Skills configured yet.' : 'No Skills match the current filters.'} />
      {:else}
        <div class="grid gap-3 xl:grid-cols-2">
          {#each filteredSkills as skill (skill.id)}
            {@const pkg = packageForSkill(skill.id)}
            {@const installation = installationForPackage(pkg?.id || '')}
            <button type="button" class="rounded-lg border bg-zinc-950/50 p-4 text-left transition hover:border-zinc-700 hover:bg-zinc-900/60 {selectedSkillId === skill.id && drawerOpen ? 'border-lime-300/50' : 'border-zinc-800'}" onclick={() => selectSkill(skill)}>
              <div class="flex items-start justify-between gap-3">
                <div class="min-w-0 space-y-2">
                  <div class="flex flex-wrap items-center gap-2"><span class="font-medium text-zinc-50">{skill.title || skill.id}</span><Badge variant={skill.enabled ? 'default' : 'secondary'}>{skill.enabled ? 'Enabled' : 'Disabled'}</Badge><Badge variant="secondary">{skill.activation_mode}</Badge></div>
                  <div class="break-all text-xs text-zinc-500">{skill.id}</div>
                  <p class="line-clamp-2 text-sm text-zinc-300">{skill.description || 'No description yet.'}</p>
                </div>
                <Badge variant={installation ? 'default' : 'secondary'}>{installation ? 'Installed' : 'Unregistered'}</Badge>
              </div>
              <div class="mt-3 flex flex-wrap gap-2"><Badge variant="secondary">{pkg?.source_kind || 'unknown'}</Badge><Badge variant={badgeVariant(pkg?.update_status || 'unknown')}>{pkg?.update_status || 'unknown'}</Badge><Badge variant={badgeVariant(pkg?.dirty_status || 'unknown')}>{pkg?.dirty_status || 'unknown'}</Badge><Badge variant={skill.instructions ? 'default' : 'outline'}>{skill.instructions ? 'Instructions' : 'No instructions'}</Badge></div>
              <div class="mt-3 grid gap-1 text-xs text-zinc-400"><div><span class="text-zinc-500">Source:</span> {sourceSummary(pkg)}</div><div><span class="text-zinc-500">Package:</span> {pkg?.id || '—'} <span class="text-zinc-600">/</span> <span class="text-zinc-500">Install:</span> {installation?.id || '—'}</div></div>
            </button>
          {/each}
        </div>
      {/if}
    </CardContent>
  </Card>

  {#if result}
    <Card><CardHeader><CardTitle>Last action result</CardTitle><CardDescription>Structured package and installation registration results.</CardDescription></CardHeader><CardContent class="space-y-3 text-sm text-zinc-300">
      {#each result.installed as item}<div class="rounded-md border border-zinc-800 bg-zinc-950/60 p-3"><div class="font-medium text-zinc-100">{item.skill_id} <Badge variant={badgeVariant(item.update_status)}>{item.update_status}</Badge></div><div class="mt-1 grid gap-1 text-xs text-zinc-400 md:grid-cols-2"><div>Package: {item.package_id}</div><div>Installation: {item.installation_id}</div><div>Source: {item.source_kind} {item.source_repo}</div><div>Verified: {Object.values(item.verification).filter(Boolean).length}/{Object.keys(item.verification).length}</div></div></div>{/each}
      {#each result.errors as item}<div class="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-red-100">{item}</div>{/each}
    </CardContent></Card>
  {/if}
</div>

<Sheet bind:open={drawerOpen}>
  <SheetContent class={drawerExpanded ? 'md:w-[min(1180px,96vw)]' : ''}>
    <SheetHeader>
      <div class="min-w-0"><SheetTitle id="skill-drawer-title" class="flex items-center gap-2"><Wrench class="size-5" /> {form.title || form.id || 'Skill editor'}</SheetTitle><SheetDescription>Edit settings, inspect package/source metadata, and run skill actions.</SheetDescription></div>
      <div class="flex gap-2"><Button variant="ghost" size="icon" onclick={() => (drawerExpanded = !drawerExpanded)} aria-label={drawerExpanded ? 'Collapse drawer' : 'Expand drawer'}>{#if drawerExpanded}<Minimize2 class="size-4" />{:else}<Maximize2 class="size-4" />{/if}</Button><Button variant="ghost" size="icon" onclick={closeDrawer} aria-label="Close skill drawer"><X class="size-4" /></Button></div>
    </SheetHeader>
    <div class="min-h-0 flex-1 overflow-y-auto p-4">
      <div class="grid gap-6 {drawerExpanded ? 'xl:grid-cols-[1.1fr_0.9fr]' : ''}">
        <section class="space-y-4"><h3 class="font-medium text-zinc-100">Overview</h3><div class="grid gap-4 sm:grid-cols-2"><div class="space-y-1"><Label for="skill-id">ID</Label><Input id="skill-id" bind:value={form.id} /></div><div class="space-y-1"><Label for="skill-title">Title</Label><Input id="skill-title" bind:value={form.title} /></div></div><div class="space-y-1"><Label for="skill-description">Description</Label><Textarea id="skill-description" bind:value={form.description} rows={3} /></div><div class="grid gap-4 sm:grid-cols-2"><div class="space-y-1"><Label for="skill-activation">Activation mode</Label><Select id="skill-activation" bind:value={form.activation_mode}><option value="manual">manual</option><option value="auto">auto</option><option value="always">always</option></Select></div><label class="mt-7 flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label></div><div class="grid gap-4 sm:grid-cols-2"><div class="space-y-1"><Label for="skill-triggers">Triggers</Label><Textarea id="skill-triggers" value={form.triggers.join('\n')} oninput={(event) => (form.triggers = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div><div class="space-y-1"><Label for="skill-tools">Required tools</Label><Textarea id="skill-tools" value={form.required_tools.join('\n')} oninput={(event) => (form.required_tools = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div><div class="space-y-1"><Label for="skill-mcps">Required MCPs</Label><Textarea id="skill-mcps" value={form.required_mcps.join('\n')} oninput={(event) => (form.required_mcps = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div><div class="space-y-1"><Label for="skill-projects">Project filters</Label><Textarea id="skill-projects" value={form.project_filters.join('\n')} oninput={(event) => (form.project_filters = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div></div><div class="space-y-1"><Label for="skill-includes">Include paths / installed file paths</Label><Textarea id="skill-includes" value={form.include_paths.join('\n')} oninput={(event) => (form.include_paths = parseList((event.currentTarget as HTMLTextAreaElement).value))} rows={3} /></div></section>
        <section class="space-y-4"><h3 class="font-medium text-zinc-100">Installation and source</h3><div class="grid gap-2 text-sm text-zinc-300">
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Package id</span><span class="break-all">{selectedPackage?.id || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Installation id</span><span class="break-all">{selectedInstallation?.id || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Scope</span><span>{selectedInstallation ? `${selectedInstallation.scope_kind} / ${selectedInstallation.scope_id}` : '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Pinned version</span><span>{selectedInstallation?.pinned_version || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Source kind</span><span>{selectedPackage?.source_kind || 'unknown'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Source URL</span><span class="break-all">{selectedPackage?.source_url || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Repo / ref</span><span class="break-all">{[selectedPackage?.source_repo, selectedPackage?.source_ref].filter(Boolean).join(' · ') || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Parent / skill path</span><span class="break-all">{[selectedPackage?.source_parent_path, selectedPackage?.source_skill_path].filter(Boolean).join(' / ') || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Source commit</span><span class="break-all">{selectedPackage?.source_commit || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Latest source commit</span><span class="break-all">{selectedPackage?.latest_source_commit || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Update / dirty</span><span>{[selectedPackage?.update_status || 'unknown', selectedPackage?.dirty_status || 'unknown'].join(' / ')}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Content checksum</span><span class="break-all">{selectedPackage?.content_checksum || '—'}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Imported at</span><span>{formatTime(selectedPackage?.imported_at)}</span></div>
            <div class="grid gap-1 rounded-md border border-zinc-800 bg-zinc-950/50 p-3"><span class="text-xs uppercase tracking-wide text-zinc-500">Last checked at</span><span>{formatTime(selectedPackage?.last_checked_at)}</span></div>
          </div><Separator /><div class="space-y-2"><div class="flex items-center justify-between"><h3 class="font-medium text-zinc-100">SKILL.md preview</h3><Badge variant={form.instructions ? 'default' : 'outline'}>{form.instructions.length} chars</Badge></div><Textarea id="skill-instructions" bind:value={form.instructions} rows={drawerExpanded ? 22 : 14} class="font-mono text-xs leading-relaxed" placeholder="# Skill name&#10;&#10;Write the instructions this skill should contribute to prompt context." /></div></section>
      </div>
    </div>
    <SheetFooter><div class="flex flex-wrap gap-2"><Button onclick={save} disabled={saving || !form.id || !form.title}>{saving ? 'Saving…' : 'Save settings'}</Button><Button variant="secondary" onclick={() => runCheckUpdates(form.id)} disabled={!form.id || pendingAction === `check:${form.id}`}><RefreshCw class="size-4" /> {pendingAction === `check:${form.id}` ? 'Checking…' : 'Check update'}</Button><Button variant="outline" disabled title="Update apply is not available yet."><CheckCircle2 class="size-4" /> Apply update soon</Button></div><Button variant="destructive" onclick={() => removeSkill(form.id)} disabled={!form.id || pendingAction === `delete:${form.id}`}><Trash2 class="size-4" /> Delete</Button></SheetFooter>
  </SheetContent>
</Sheet>

<Dialog bind:open={reconcileOpen}>
  <DialogContent>
    <div class="flex items-start justify-between gap-4">
      <div>
        <DialogTitle id="reconcile-title">Scan local skill folders</DialogTitle>
        <DialogDescription>
          This scans the Nucleus state <code>skills/</code> directory and only registers the folders you select. It does not prune deleted skills or decide what should be enabled.
        </DialogDescription>
      </div>
      <Button variant="ghost" size="icon" onclick={() => (reconcileOpen = false)} aria-label="Close local skill scan"><X class="size-4" /></Button>
    </div>

    <div class="mt-5 space-y-4">
      <div class="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-100">
        Registering a folder adds it to the catalog as an unknown-source workspace skill. Review the list first; a large state directory can contain many old or experimental skills.
      </div>

      {#if pendingAction === 'reconcile-scan'}
        <div class="text-sm text-zinc-400">Scanning local skill folders…</div>
      {:else if reconcileScan}
        <div class="space-y-1 text-sm text-zinc-400">
          <div>Scanned: <span class="break-all text-zinc-200">{reconcileScan.skills_dir}</span></div>
          <div>Found {reconcileScan.candidates.length} skill folder{reconcileScan.candidates.length === 1 ? '' : 's'} · {reconcileScan.candidates.filter((candidate) => !candidate.already_registered).length} not registered.</div>
        </div>

        <div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_12rem]">
          <Input placeholder="Filter found folders…" bind:value={reconcileCandidateSearch} />
          <Select bind:value={reconcileCandidateFilter} aria-label="Local skill folder filter">
            <option value="unregistered">Not registered</option>
            <option value="registered">Registered</option>
            <option value="all">All found</option>
          </Select>
        </div>

        {#if reconcileScan.errors.length > 0}
          <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-200">
            {reconcileScan.errors.join(' · ')}
          </div>
        {/if}

        {#if reconcileScan.candidates.length === 0}
          <WorkspaceEmptyState message="No local SKILL.md folders were found." />
        {:else if filteredReconcileCandidates.length === 0}
          <WorkspaceEmptyState message={reconcileCandidateFilter === 'unregistered' ? 'No unregistered local skill folders were found.' : 'No local skill folders match the current filter.'} />
        {:else}
          <div class="flex flex-wrap items-center justify-between gap-2 text-xs text-zinc-500">
            <span>Showing {filteredReconcileCandidates.length} of {reconcileScan.candidates.length}</span>
            <div class="flex gap-2">
              <Button variant="ghost" size="sm" onclick={selectVisibleReconcileSkills}>Select visible unregistered</Button>
              <Button variant="ghost" size="sm" onclick={clearVisibleReconcileSkills}>Clear visible</Button>
            </div>
          </div>
          <div class="max-h-80 space-y-2 overflow-y-auto rounded-lg border border-zinc-800 p-2">
            {#each filteredReconcileCandidates as candidate, index (`${candidate.path}:${index}`)}
              <label class="flex cursor-pointer items-start gap-3 rounded-md px-2 py-2 hover:bg-zinc-900/70">
                <input
                  type="checkbox"
                  class="mt-1"
                  checked={selectedReconcileSkillIds.includes(candidate.skill_id)}
                  onchange={(event) => toggleReconcileSkill(candidate.skill_id, (event.currentTarget as HTMLInputElement).checked)}
                />
                <span class="min-w-0 flex-1">
                  <span class="flex flex-wrap items-center gap-2">
                    <span class="font-medium text-zinc-100">{candidate.title}</span>
                    {#if candidate.already_registered}<Badge variant="secondary">Registered</Badge>{/if}
                  </span>
                  <span class="mt-1 block break-all text-xs text-zinc-500">{candidate.path}</span>
                </span>
              </label>
            {/each}
          </div>
        {/if}
      {/if}
    </div>

    <div class="mt-5 flex flex-wrap items-center justify-between gap-2">
      <div class="text-xs text-zinc-500">{selectedReconcileSkillIds.length} selected</div>
      <div class="flex gap-2">
        <Button variant="secondary" onclick={() => (reconcileOpen = false)}>Cancel</Button>
        <Button onclick={runReconcile} disabled={!reconcileScan || selectedReconcileSkillIds.length === 0 || pendingAction === 'reconcile' || pendingAction === 'reconcile-scan'}>{pendingAction === 'reconcile' ? 'Registering…' : 'Register selected'}</Button>
      </div>
    </div>
  </DialogContent>
</Dialog>

<Dialog bind:open={importOpen}>
  <DialogContent>
    <div class="flex items-start justify-between"><div><DialogTitle id="import-title">Import skills</DialogTitle><DialogDescription>Import copies skill files into Nucleus, registers manifests/packages/installations, and stores repo metadata for update checks.</DialogDescription></div><Button variant="ghost" size="icon" onclick={() => (importOpen = false)} aria-label="Close import dialog"><X class="size-4" /></Button></div>
    <div class="mt-5 space-y-4"><div class="space-y-1"><Label for="import-source">Local path or GitHub tree URL</Label><Input id="import-source" bind:value={importSource} placeholder="https://github.com/coreyhaines31/marketingskills/tree/main/skills" /></div><div class="grid gap-4 sm:grid-cols-2"><div class="space-y-1"><Label for="scope-kind">Scope kind</Label><Input id="scope-kind" bind:value={importScopeKind} /></div><div class="space-y-1"><Label for="scope-id">Scope id</Label><Input id="scope-id" bind:value={importScopeId} /></div></div><p class="text-xs text-zinc-500">Copying folders manually is not enough; import registers the package and installation records Nucleus uses.</p></div>
    <div class="mt-5 flex justify-end gap-2"><Button variant="secondary" onclick={() => (importOpen = false)}>Cancel</Button><Button onclick={runImport} disabled={!importSource.trim() || pendingAction === 'import'}>{pendingAction === 'import' ? 'Importing…' : 'Import skills'}</Button></div>
  </DialogContent>
</Dialog>
