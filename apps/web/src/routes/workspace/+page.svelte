<script lang="ts">
  import { WorkspacePageHeader } from '$lib/components/app/workspace';
  import { onMount } from 'svelte';
  import {
    AlertTriangle,
    Bot,
    CheckCircle2,
    Cpu,
    KeyRound,
    Link2,
    Loader2,
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
    checkWorkspaceProfile,
    createWorkspaceProfile,
    deleteWorkspaceProfile,
    fetchOverview,
    updateWorkspaceProfile
  } from '$lib/nucleus/client';
  import { compactPath, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    ProfileCheckResult,
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

  type ModelRole = 'main' | 'utility';

  type ProfileCheckState = {
    pending: boolean;
    result: ProfileCheckResult | null;
    error: string | null;
    signature: string | null;
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
  const unknownModelCapabilities = {
    json_object: 'unknown' as const,
    transport: 'unknown' as const,
    action_contract: 'unknown' as const
  };

  let overview = $state<RuntimeOverview | null>(null);
  let defaultProfileId = $state('');
  let selectedProfileId = $state('');
  let profileDrafts = $state<WorkspaceProfileSummary[]>([]);
  let loading = $state(true);
  let refreshing = $state(false);
  let savingProfileId = $state<string | null>(null);
  let deletingProfileId = $state<string | null>(null);
  let creatingProfile = $state(false);
  let profileChecks = $state<Record<string, ProfileCheckState>>({});
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
    pruneStaleProfileChecks(nextDrafts);
    profileDrafts = nextDrafts;
    selectedProfileId = nextSelectedProfileId;
  }

  function pruneStaleProfileChecks(nextDrafts: WorkspaceProfileSummary[]) {
    const nextChecks = { ...profileChecks };
    let changed = false;

    for (const [key, state] of Object.entries(profileChecks)) {
      const [profileId, rawRole] = key.split(':');
      const role: ModelRole | null = rawRole === 'main' || rawRole === 'utility' ? rawRole : null;
      const profile = nextDrafts.find((item) => item.id === profileId);
      const signature = profile && role ? modelCheckSignature(profile[role]) : null;

      if (!signature || state.signature !== signature) {
        delete nextChecks[key];
        changed = true;
      }
    }

    if (changed) {
      profileChecks = nextChecks;
    }
  }

  function profileSignature(profile: WorkspaceProfileSummary, selectedDefaultProfileId: string) {
    return JSON.stringify({
      title: profile.title,
      is_default: profile.id === selectedDefaultProfileId,
      main: editableModelConfig(profile.main),
      utility: editableModelConfig(profile.utility)
    });
  }

  function editableModelConfig(config: WorkspaceModelConfig) {
    return {
      adapter: config.adapter,
      model: config.model,
      base_url: config.base_url,
      api_key: config.api_key
    };
  }

  function modelTargetMatches(left: WorkspaceModelConfig, right: WorkspaceModelConfig) {
    return (
      left.adapter === right.adapter &&
      left.model.trim() === right.model.trim() &&
      left.base_url.trim().replace(/\/+$/, '') === right.base_url.trim().replace(/\/+$/, '')
    );
  }

  function modelConfigForSave(draft: WorkspaceModelConfig, source?: WorkspaceModelConfig) {
    if (!source || !modelTargetMatches(draft, source)) {
      return draft;
    }

    return {
      ...draft,
      json_object: source.json_object,
      transport: source.transport,
      action_contract: source.action_contract
    };
  }

  function applyProfileCheckCapabilities(
    profileId: string,
    role: ModelRole,
    result: ProfileCheckResult
  ) {
    const applyToProfile = (profile: WorkspaceProfileSummary): WorkspaceProfileSummary =>
      profile.id === profileId
        ? {
            ...profile,
            [role]: {
              ...profile[role],
              json_object: result.json_object,
              transport: result.transport,
              action_contract: result.action_contract
            }
          }
        : profile;

    profileDrafts = profileDrafts.map(applyToProfile);
    if (overview) {
      overview = {
        ...overview,
        workspace: {
          ...overview.workspace,
          profiles: overview.workspace.profiles.map(applyToProfile)
        }
      };
    }
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

  function profileCheckKey(profileId: string, role: ModelRole) {
    return `${profileId}:${role}`;
  }

  function profileCheckFor(profileId: string, role: ModelRole): ProfileCheckState {
    return (
      profileChecks[profileCheckKey(profileId, role)] ?? {
        pending: false,
        result: null,
        error: null,
        signature: null
      }
    );
  }

  function setProfileCheck(profileId: string, role: ModelRole, state: ProfileCheckState) {
    profileChecks = {
      ...profileChecks,
      [profileCheckKey(profileId, role)]: state
    };
  }

  function clearProfileCheck(profileId: string, role: ModelRole) {
    const key = profileCheckKey(profileId, role);
    if (!(key in profileChecks)) {
      return;
    }
    const next = { ...profileChecks };
    delete next[key];
    profileChecks = next;
  }

  function profileCheckStatusClass(state: ProfileCheckState) {
    if (state.pending) {
      return 'border-amber-300/30 bg-amber-300/10 text-amber-100';
    }
    if (state.error || (state.result && state.result.outcome !== 'ok')) {
      return 'border-red-500/30 bg-red-500/10 text-red-200';
    }
    if (state.result?.outcome === 'ok') {
      return 'border-emerald-400/30 bg-emerald-400/10 text-emerald-100';
    }
    return 'border-zinc-800 bg-zinc-950/60 text-zinc-500';
  }

  function profileCheckMessage(state: ProfileCheckState) {
    if (state.pending) {
      return 'Checking connection...';
    }
    if (state.error) {
      return state.error;
    }
    if (state.result) {
      const latency = state.result.latency_ms === undefined ? '' : ` (${state.result.latency_ms} ms)`;
      return `${state.result.message}${latency}`;
    }
    return '';
  }

  function profileCheckFingerprint(result: ProfileCheckResult) {
    return `Transport ${formatState(result.transport)} / Action ${formatState(result.action_contract)} / JSON ${formatState(result.json_object)}`;
  }

  function modelCheckSignature(config: WorkspaceModelConfig) {
    return JSON.stringify({
      adapter: config.adapter,
      model: config.model.trim(),
      base_url: config.base_url.trim().replace(/\/+$/, ''),
      api_key: config.api_key
    });
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
    role: ModelRole,
    updater: (config: WorkspaceModelConfig) => WorkspaceModelConfig
  ) {
    clearProfileCheck(profileId, role);
    updateProfileDraft(profileId, (profile) => ({
      ...profile,
      [role]: updater({ ...profile[role] })
    }));
  }

  async function handleCheckProfile(profileId: string, role: ModelRole) {
    const profile = profileDrafts.find((item) => item.id === profileId);
    if (!profile) {
      return;
    }
    const signature = modelCheckSignature(profile[role]);
    setProfileCheck(profileId, role, { pending: true, result: null, error: null, signature });

    try {
      const result = await checkWorkspaceProfile(profileId, role);
      if (profileCheckFor(profileId, role).signature !== signature) {
        return;
      }
      setProfileCheck(profileId, role, { pending: false, result, error: null, signature });
      if (result.outcome === 'ok') {
        applyProfileCheckCapabilities(profileId, role, result);
      }
      error = null;
    } catch (cause) {
      if (profileCheckFor(profileId, role).signature !== signature) {
        return;
      }
      setProfileCheck(profileId, role, {
        pending: false,
        result: null,
        error: cause instanceof Error ? cause.message : 'Connection check failed.',
        signature
      });
    }
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
      api_key: '',
      ...unknownModelCapabilities
    };
    const baseUtility = template?.utility ?? {
      adapter: 'codex',
      model: '',
      base_url: '',
      api_key: '',
      ...unknownModelCapabilities
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
      const source = workspace?.profiles.find((item) => item.id === profile.id);
      const saved = await updateWorkspaceProfile(profileId, {
        title: profile.title,
        main: modelConfigForSave(profile.main, source?.main),
        utility: modelConfigForSave(profile.utility, source?.utility),
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
                {@const checkState = profileCheckFor(selectedProfile.id, modelRole.key)}
                <div class="rounded-xl border border-zinc-800 bg-zinc-950/40 p-4">
                  <div class="mb-4 flex flex-wrap items-center justify-between gap-3">
                    <div class="flex items-center gap-2">
                      <modelRole.icon class="size-4 text-zinc-500" />
                      <div class="text-sm font-medium text-zinc-100">{modelRole.title}</div>
                    </div>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={selectedProfileDirty || checkState.pending}
                      title={selectedProfileDirty ? 'Save changes before checking.' : 'Check connection'}
                      onclick={() => handleCheckProfile(selectedProfile.id, modelRole.key)}
                    >
                      {#if checkState.pending}
                        <Loader2 class="size-4 animate-spin" />
                        Checking
                      {:else}
                        <CheckCircle2 class="size-4" />
                        Check connection
                      {/if}
                    </Button>
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
                              api_key: adapterNeedsBaseUrl(adapter) ? current.api_key : '',
                              ...unknownModelCapabilities
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
                            model: (event.currentTarget as HTMLInputElement).value,
                            ...unknownModelCapabilities
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
                              base_url: (event.currentTarget as HTMLInputElement).value,
                              ...unknownModelCapabilities
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

                    {#if selectedProfileDirty || checkState.pending || checkState.result || checkState.error}
                      <div
                        class={`flex items-start gap-2 rounded-md border px-3 py-2 text-xs ${
                          selectedProfileDirty
                            ? 'border-amber-300/30 bg-amber-300/10 text-amber-100'
                            : profileCheckStatusClass(checkState)
                        }`}
                      >
                        {#if checkState.pending}
                          <Loader2 class="mt-0.5 size-3.5 shrink-0 animate-spin" />
                        {:else if checkState.result?.outcome === 'ok'}
                          <CheckCircle2 class="mt-0.5 size-3.5 shrink-0" />
                        {:else}
                          <AlertTriangle class="mt-0.5 size-3.5 shrink-0" />
                        {/if}
                        <span class="space-y-1">
                          {selectedProfileDirty ? 'Save changes before checking.' : profileCheckMessage(checkState)}
                          {#if checkState.result}
                            <span class="block opacity-80">{profileCheckFingerprint(checkState.result)}</span>
                          {/if}
                        </span>
                      </div>
                    {/if}
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
