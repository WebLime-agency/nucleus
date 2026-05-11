<script lang="ts">
  import { WorkspacePageHeader } from '$lib/components/app/workspace';
  import { onMount } from 'svelte';
  import {
    Bot,
    Cpu,
    KeyRound,
    Link2,
    Plus,
    Save,
    Settings2,
    Trash2
  } from 'lucide-svelte';

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import {
    createWorkspaceProfile,
    deleteWorkspaceProfile,
    fetchOverview,
    updateWorkspaceProfile
  } from '$lib/nucleus/client';
  import { compactPath, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    RuntimeOverview,
    WorkspaceModelConfig,
    WorkspaceProfileSummary,
    WorkspaceSummary
  } from '$lib/nucleus/schemas';

  type AdapterOption = {
    value: WorkspaceModelConfig['adapter'];
    label: string;
    helper: string;
  };

  const adapterOptions: AdapterOption[] = [
    {
      value: 'claude',
      label: 'Claude CLI',
      helper: 'Uses the local Claude CLI session runtime.'
    },
    {
      value: 'codex',
      label: 'Codex CLI',
      helper: 'Uses the local Codex CLI session runtime.'
    },
    {
      value: 'openai_compatible',
      label: 'OpenAI-compatible',
      helper: 'Works with 9Router, OpenRouter, LM Studio, OpenAI-compatible gateways, and similar APIs.'
    }
  ];

  let overview = $state<RuntimeOverview | null>(null);
  let defaultProfileId = $state('');
  let selectedProfileId = $state('');
  let profileDrafts = $state<WorkspaceProfileSummary[]>([]);
  let loading = $state(true);
  let refreshing = $state(false);
  let savingProfileId = $state<string | null>(null);
  let deletingProfileId = $state<string | null>(null);
  let creatingProfile = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  let workspace = $derived(overview?.workspace ?? null);
  let selectedProfile = $derived(
    profileDrafts.find((profile) => profile.id === selectedProfileId) ?? profileDrafts[0] ?? null
  );
  let hasDirtyProfiles = $derived.by(() =>
    workspace
      ? profileDrafts.some((profile) => profileIsDirty(profile, workspace))
      : false
  );
  let selectedProfileDirty = $derived(
    workspace && selectedProfile ? profileIsDirty(selectedProfile, workspace) : false
  );

  function cloneProfile(profile: WorkspaceProfileSummary): WorkspaceProfileSummary {
    return {
      ...profile,
      main: { ...profile.main },
      utility: { ...profile.utility }
    };
  }

  function syncWorkspaceFields(nextWorkspace: WorkspaceSummary, force = false) {
    if (!force && hasDirtyProfiles) {
      return;
    }

    const nextDrafts = nextWorkspace.profiles.map(cloneProfile);
    const nextSelectedProfileId = nextDrafts.some((profile) => profile.id === selectedProfileId)
      ? selectedProfileId
      : (nextDrafts[0]?.id ?? '');

    defaultProfileId = nextWorkspace.default_profile_id;
    profileDrafts = nextDrafts;
    selectedProfileId = nextSelectedProfileId;
  }

  function profileSignature(profile: WorkspaceProfileSummary, selectedDefaultProfileId: string) {
    return JSON.stringify({
      title: profile.title,
      is_default: profile.id === selectedDefaultProfileId,
      main: profile.main,
      utility: profile.utility
    });
  }

  function profileIsDirty(profile: WorkspaceProfileSummary, currentWorkspace: WorkspaceSummary) {
    const source = currentWorkspace.profiles.find((item) => item.id === profile.id);
    if (!source) {
      return true;
    }

    return (
      profileSignature(profile, defaultProfileId) !==
      profileSignature(source, currentWorkspace.default_profile_id)
    );
  }

  function helperForAdapter(adapter: string) {
    return adapterOptions.find((option) => option.value === adapter)?.helper ?? 'Unknown adapter.';
  }

  function adapterNeedsBaseUrl(adapter: string) {
    return adapter === 'openai_compatible';
  }

  function adapterLabel(adapter: string) {
    return adapterOptions.find((option) => option.value === adapter)?.label ?? formatState(adapter);
  }

  function modelSummary(config: WorkspaceModelConfig) {
    if (config.adapter === 'openai_compatible') {
      return config.model.trim()
        ? `${config.model.trim()} via ${config.base_url || 'custom gateway'}`
        : config.base_url || 'Custom gateway';
    }

    return config.model.trim() || 'Use provider default';
  }

  function updateProfileDraft(
    profileId: string,
    updater: (profile: WorkspaceProfileSummary) => WorkspaceProfileSummary
  ) {
    profileDrafts = profileDrafts.map((profile) =>
      profile.id === profileId ? updater(cloneProfile(profile)) : profile
    );
  }

  function updateModelDraft(
    profileId: string,
    role: 'main' | 'utility',
    updater: (config: WorkspaceModelConfig) => WorkspaceModelConfig
  ) {
    updateProfileDraft(profileId, (profile) => ({
      ...profile,
      [role]: updater({ ...profile[role] })
    }));
  }

  async function loadAll(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      const nextOverview = await fetchOverview();
      overview = nextOverview;
      syncWorkspaceFields(nextOverview.workspace, true);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to read workspace state.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function handleCreateProfile() {
    const template = workspace?.profiles.find((profile) => profile.id === defaultProfileId) ??
      workspace?.profiles[0];

    const baseMain = template?.main ?? {
      adapter: 'claude',
      model: 'sonnet',
      base_url: '',
      api_key: ''
    };
    const baseUtility = template?.utility ?? {
      adapter: 'codex',
      model: '',
      base_url: '',
      api_key: ''
    };

    creatingProfile = true;
    success = null;

    try {
      const profile = await createWorkspaceProfile({
        title: 'New Profile',
        main: baseMain,
        utility: baseUtility,
        is_default: false
      });

      if (workspace) {
        const nextWorkspace = {
          ...workspace,
          profiles: [profile, ...workspace.profiles]
        };
        overview = overview ? { ...overview, workspace: nextWorkspace } : null;
        syncWorkspaceFields(nextWorkspace, true);
      } else {
        await loadAll(true);
      }

      selectedProfileId = profile.id;
      success = 'Profile created.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to create the profile.';
    } finally {
      creatingProfile = false;
    }
  }

  async function handleSaveProfile(profileId: string) {
    const profile = profileDrafts.find((item) => item.id === profileId);
    if (!profile) {
      return;
    }

    savingProfileId = profileId;
    success = null;

    try {
      const saved = await updateWorkspaceProfile(profileId, {
        title: profile.title,
        main: profile.main,
        utility: profile.utility,
        is_default: profile.id === defaultProfileId
      });

      if (workspace) {
        const effectiveDefaultProfileId = saved.is_default ? saved.id : defaultProfileId;
        const nextProfiles = workspace.profiles.map((item) => {
          if (item.id === saved.id) {
            return saved;
          }

          return {
            ...item,
            is_default: item.id === effectiveDefaultProfileId
          };
        });
        const nextWorkspace = {
          ...workspace,
          default_profile_id: effectiveDefaultProfileId,
          profiles: nextProfiles
        };
        overview = overview ? { ...overview, workspace: nextWorkspace } : null;
        syncWorkspaceFields(nextWorkspace, true);
      } else {
        await loadAll(true);
      }

      selectedProfileId = saved.id;
      success = `Saved ${saved.title}.`;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save the profile.';
    } finally {
      savingProfileId = null;
    }
  }

  async function handleDeleteProfile(profileId: string) {
    deletingProfileId = profileId;
    success = null;

    try {
      const nextWorkspace = await deleteWorkspaceProfile(profileId);
      overview = overview ? { ...overview, workspace: nextWorkspace } : null;
      syncWorkspaceFields(nextWorkspace, true);
      selectedProfileId = nextWorkspace.profiles[0]?.id ?? '';
      success = 'Profile deleted.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete the profile.';
    } finally {
      deletingProfileId = null;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event !== 'overview.updated') {
      return;
    }

    overview = event.data;
    syncWorkspaceFields(event.data.workspace);
    loading = false;
    refreshing = false;
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
  <title>Nucleus - Profiles</title>
</svelte:head>

<div class="space-y-8">
  <section class="space-y-3">
    <div>
      <h1 class="text-3xl font-semibold text-zinc-50">Profiles</h1>
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
        Pick a profile, choose its main and utility models, save, and let new sessions inherit the
        result. The runtime inventory below shows which adapters Nucleus can actually drive.
      </p>
    </div>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  {#if success}
    <div class="rounded-lg border border-lime-300/30 bg-lime-300/10 px-4 py-3 text-sm text-lime-100">
      {success}
    </div>
  {/if}

  <section class="grid gap-4 xl:grid-cols-[18rem_minmax(0,1fr)]">
    <Card>
      <CardHeader>
        <CardTitle>Profiles</CardTitle>
        <CardDescription>
          Select one profile, edit it, save it, then move on. This stays compact even when the list grows.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-4">
        <label class="block space-y-1">
          <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Active Profile</span>
          <select
            class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
            bind:value={selectedProfileId}
            disabled={profileDrafts.length === 0}
          >
            {#if profileDrafts.length === 0}
              <option value="">No profiles available</option>
            {:else}
              {#each profileDrafts as profile}
                <option value={profile.id}>{profile.title}</option>
              {/each}
            {/if}
          </select>
        </label>

        <Button variant="outline" onclick={handleCreateProfile} disabled={creatingProfile}>
          <Plus class={creatingProfile ? 'size-4 animate-spin' : 'size-4'} />
          {creatingProfile ? 'Creating' : 'Add Profile'}
        </Button>

        {#if selectedProfile}
          <div class="space-y-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4">
            <div>
              <div class="text-sm font-medium text-zinc-100">{selectedProfile.title || 'Untitled Profile'}</div>
              <div class="mt-1 flex flex-wrap items-center gap-2">
                {#if selectedProfile.id === defaultProfileId}
                  <Badge>Default</Badge>
                {/if}
                {#if workspace && selectedProfileDirty}
                  <Badge variant="secondary">Unsaved</Badge>
                {/if}
              </div>
            </div>
            <div class="space-y-2 text-xs text-zinc-500">
              <div>Main: {adapterLabel(selectedProfile.main.adapter)} - {modelSummary(selectedProfile.main)}</div>
              <div>Utility: {adapterLabel(selectedProfile.utility.adapter)} - {modelSummary(selectedProfile.utility)}</div>
            </div>
          </div>
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader class="gap-4 lg:flex-row lg:items-start lg:justify-between">
        <div class="space-y-3">
          <div class="flex flex-wrap items-center gap-2">
            <CardTitle>{selectedProfile?.title || 'Profile Editor'}</CardTitle>
            {#if selectedProfile && selectedProfile.id === defaultProfileId}
              <Badge>Default</Badge>
            {/if}
            {#if workspace && selectedProfileDirty}
              <Badge variant="secondary">Unsaved</Badge>
            {/if}
          </div>
          <CardDescription>
            Main model settings drive the session. Utility model settings stay inside Nucleus for prompt
            assembly, routing, and background work.
          </CardDescription>
        </div>

        {#if selectedProfile}
          <div class="flex flex-wrap gap-2">
            {#if selectedProfile.id !== defaultProfileId}
              <Button
                variant="secondary"
                size="sm"
                onclick={() => {
                  defaultProfileId = selectedProfile.id;
                }}
              >
                Make Default
              </Button>
            {/if}
            <Button
              variant="outline"
              size="sm"
              disabled={!workspace || !selectedProfileDirty || savingProfileId === selectedProfile.id}
              onclick={() => handleSaveProfile(selectedProfile.id)}
            >
              <Save class={savingProfileId === selectedProfile.id ? 'size-4 animate-spin' : 'size-4'} />
              {savingProfileId === selectedProfile.id ? 'Saving' : 'Save'}
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={profileDrafts.length <= 1 || deletingProfileId === selectedProfile.id}
              onclick={() => handleDeleteProfile(selectedProfile.id)}
            >
              <Trash2 class={deletingProfileId === selectedProfile.id ? 'size-4 animate-pulse' : 'size-4'} />
              Delete
            </Button>
          </div>
        {/if}
      </CardHeader>

      <CardContent>
        {#if !selectedProfile}
          <div class="rounded-md border border-dashed border-zinc-800 px-4 py-8 text-sm text-zinc-500">
            No workspace profiles are configured yet.
          </div>
        {:else}
          <div class="space-y-5">
            <label class="block space-y-1">
              <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Title</span>
              <input
                class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                value={selectedProfile.title}
                oninput={(event) =>
                  updateProfileDraft(selectedProfile.id, (current) => ({
                    ...current,
                    title: (event.currentTarget as HTMLInputElement).value
                  }))}
              />
            </label>

            <div class="grid gap-4 xl:grid-cols-2">
              {#each [
                { key: 'main' as const, title: 'Main Model', icon: Bot },
                { key: 'utility' as const, title: 'Utility Model', icon: Settings2 }
              ] as modelRole}
                <div class="rounded-xl border border-zinc-800 bg-zinc-950/40 p-4">
                  <div class="mb-4 flex items-center gap-2">
                    <modelRole.icon class="size-4 text-zinc-500" />
                    <div class="text-sm font-medium text-zinc-100">{modelRole.title}</div>
                  </div>

                  <div class="space-y-4">
                    <label class="block space-y-1">
                      <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Adapter</span>
                      <select
                        class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                        value={selectedProfile[modelRole.key].adapter}
                        onchange={(event) =>
                          updateModelDraft(selectedProfile.id, modelRole.key, (current) => {
                            const adapter = (event.currentTarget as HTMLSelectElement).value;
                            return {
                              ...current,
                              adapter,
                              base_url: adapterNeedsBaseUrl(adapter) ? current.base_url : '',
                              api_key: adapterNeedsBaseUrl(adapter) ? current.api_key : ''
                            };
                          })}
                      >
                        {#each adapterOptions as option}
                          <option value={option.value}>{option.label}</option>
                        {/each}
                      </select>
                      <div class="text-xs text-zinc-500">
                        {helperForAdapter(selectedProfile[modelRole.key].adapter)}
                      </div>
                    </label>

                    <label class="block space-y-1">
                      <span class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Model</span>
                      <input
                        class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                        value={selectedProfile[modelRole.key].model}
                        placeholder={
                          selectedProfile[modelRole.key].adapter === 'claude'
                            ? 'sonnet'
                            : selectedProfile[modelRole.key].adapter === 'codex'
                              ? 'gpt-5.4'
                              : 'gpt-4.1-mini'
                        }
                        oninput={(event) =>
                          updateModelDraft(selectedProfile.id, modelRole.key, (current) => ({
                            ...current,
                            model: (event.currentTarget as HTMLInputElement).value
                          }))}
                      />
                    </label>

                    {#if adapterNeedsBaseUrl(selectedProfile[modelRole.key].adapter)}
                      <label class="block space-y-1">
                        <span class="inline-flex items-center gap-1 text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">
                          <Link2 class="size-3.5" />
                          Base URL
                        </span>
                        <input
                          class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                          value={selectedProfile[modelRole.key].base_url}
                          placeholder="http://mini-server:20128/v1"
                          oninput={(event) =>
                            updateModelDraft(selectedProfile.id, modelRole.key, (current) => ({
                              ...current,
                              base_url: (event.currentTarget as HTMLInputElement).value
                            }))}
                        />
                      </label>

                      <label class="block space-y-1">
                        <span class="inline-flex items-center gap-1 text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">
                          <KeyRound class="size-3.5" />
                          API Key
                        </span>
                        <input
                          type="password"
                          class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                          value={selectedProfile[modelRole.key].api_key}
                          placeholder="Optional for local gateways"
                          oninput={(event) =>
                            updateModelDraft(selectedProfile.id, modelRole.key, (current) => ({
                              ...current,
                              api_key: (event.currentTarget as HTMLInputElement).value
                            }))}
                        />
                      </label>
                    {/if}

                    <div class="rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-2 text-xs text-zinc-500">
                      {adapterLabel(selectedProfile[modelRole.key].adapter)} - {modelSummary(selectedProfile[modelRole.key])}
                    </div>
                  </div>
                </div>
              {/each}
            </div>
          </div>
        {/if}
      </CardContent>
    </Card>
  </section>
</div>
