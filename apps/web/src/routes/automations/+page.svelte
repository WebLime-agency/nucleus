<script lang="ts">
  import { onMount } from 'svelte';
  import {
    CalendarClock,
    Play,
    Plus,
    RefreshCw,
    Save,
    TimerReset,
    Trash2,
    Workflow
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
    createPlaybook,
    deletePlaybook,
    fetchJobDetail,
    fetchOverview,
    fetchPlaybookDetail,
    fetchPlaybooks,
    runPlaybook,
    updatePlaybook
  } from '$lib/nucleus/client';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    JobDetail,
    JobSummary,
    PlaybookDetail,
    PlaybookSummary,
    RuntimeOverview,
    WorkspaceProfileSummary
  } from '$lib/nucleus/schemas';

  type PolicyBundleOption = {
    value: string;
    label: string;
    summary: string;
  };

  const policyBundles: PolicyBundleOption[] = [
    {
      value: 'read_only',
      label: 'Read Only',
      summary: 'Inspection tools only.'
    },
    {
      value: 'repo_mutation',
      label: 'Repo Mutation',
      summary: 'Read-only plus file and git write tools, still approval-gated.'
    },
    {
      value: 'command_runner',
      label: 'Command Runner',
      summary: 'Read-only plus bounded command and test execution.'
    },
    {
      value: 'full_agent',
      label: 'Full Agent',
      summary: 'Read, mutate, and run bounded commands through Nucleus.'
    }
  ];

  const triggerOptions = [
    { value: 'manual', label: 'Manual only' },
    { value: 'schedule', label: 'Scheduled interval' },
    { value: 'event', label: 'Event trigger' }
  ];

  const eventOptions = [
    { value: 'daemon_started', label: 'Nucleus started' },
    { value: 'workspace_projects_synced', label: 'Workspace projects synced' }
  ];

  let overview = $state<RuntimeOverview | null>(null);
  let playbooks = $state<PlaybookSummary[]>([]);
  let playbookDetail = $state<PlaybookDetail | null>(null);
  let selectedPlaybookId = $state('');
  let selectedJobId = $state('');
  let selectedJobDetail = $state<JobDetail | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let saving = $state(false);
  let creating = $state(false);
  let deleting = $state(false);
  let running = $state(false);
  let jobLoading = $state(false);
  let streamStatus = $state<StreamStatus>('connecting');
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);

  let draftTitle = $state('');
  let draftDescription = $state('');
  let draftPrompt = $state('');
  let draftProfileId = $state('');
  let draftProjectId = $state('');
  let draftPolicyBundle = $state('read_only');
  let draftTriggerKind = $state('manual');
  let draftScheduleIntervalSecs = $state('900');
  let draftEventKind = $state('daemon_started');
  let draftEnabled = $state(true);

  let workspace = $derived(overview?.workspace ?? null);
  let workspaceProfiles = $derived(workspace?.profiles ?? []);
  let workspaceProjects = $derived(workspace?.projects ?? []);
  let selectedPlaybook = $derived(
    playbookDetail?.playbook ??
      playbooks.find((playbook) => playbook.id === selectedPlaybookId) ??
      playbooks[0] ??
      null
  );
  let selectedProfile = $derived(
    workspaceProfiles.find((profile) => profile.id === draftProfileId) ?? null
  );
  let selectedBundle = $derived(
    policyBundles.find((bundle) => bundle.value === draftPolicyBundle) ?? policyBundles[0]
  );
  let selectedRecentJob = $derived(
    playbookDetail?.recent_jobs.find((job) => job.id === selectedJobId) ??
      playbookDetail?.recent_jobs[0] ??
      null
  );
  let draftDirty = $derived.by(() => {
    if (!playbookDetail) {
      return false;
    }

    return JSON.stringify({
      title: draftTitle,
      description: draftDescription,
      prompt: draftPrompt,
      profile_id: draftProfileId,
      project_id: draftProjectId,
      policy_bundle: draftPolicyBundle,
      trigger_kind: draftTriggerKind,
      schedule_interval_secs: draftScheduleIntervalSecs,
      event_kind: draftEventKind,
      enabled: draftEnabled
    }) !== JSON.stringify({
      title: playbookDetail.playbook.title,
      description: playbookDetail.playbook.description,
      prompt: playbookDetail.prompt,
      profile_id: playbookDetail.playbook.profile_id,
      project_id: playbookDetail.playbook.project_id,
      policy_bundle: playbookDetail.playbook.policy_bundle,
      trigger_kind: playbookDetail.playbook.trigger_kind,
      schedule_interval_secs: String(playbookDetail.playbook.schedule_interval_secs ?? 900),
      event_kind: playbookDetail.playbook.event_kind ?? 'daemon_started',
      enabled: playbookDetail.playbook.enabled
    });
  });
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (refreshing) return 'Refreshing';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (error) return 'Degraded';
    return 'Live';
  });

  function badgeVariantForJobState(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'completed' || state === 'approved') return 'default';
    if (state === 'running' || state === 'queued' || state === 'paused' || state === 'pending') {
      return 'warning';
    }
    if (state === 'canceled') return 'secondary';
    return 'destructive';
  }

  function hydrateDraft(detail: PlaybookDetail) {
    draftTitle = detail.playbook.title;
    draftDescription = detail.playbook.description;
    draftPrompt = detail.prompt;
    draftProfileId = detail.playbook.profile_id;
    draftProjectId = detail.playbook.project_id;
    draftPolicyBundle = detail.playbook.policy_bundle;
    draftTriggerKind = detail.playbook.trigger_kind;
    draftScheduleIntervalSecs = String(detail.playbook.schedule_interval_secs ?? 900);
    draftEventKind = detail.playbook.event_kind ?? 'daemon_started';
    draftEnabled = detail.playbook.enabled;
  }

  function syncPlaybookSummary(next: PlaybookSummary) {
    const remaining = playbooks.filter((playbook) => playbook.id !== next.id);
    playbooks = [next, ...remaining].sort((left, right) => {
      if (right.updated_at !== left.updated_at) {
        return right.updated_at - left.updated_at;
      }

      return right.created_at - left.created_at;
    });
  }

  function syncPlaybookDetail(next: PlaybookDetail | null) {
    playbookDetail = next;
    if (!next) {
      selectedPlaybookId = '';
      selectedJobId = '';
      selectedJobDetail = null;
      return;
    }

    selectedPlaybookId = next.playbook.id;
    syncPlaybookSummary(next.playbook);
    hydrateDraft(next);
    const fallbackJobId = next.recent_jobs[0]?.id ?? '';
    if (!next.recent_jobs.some((job) => job.id === selectedJobId)) {
      selectedJobId = fallbackJobId;
      selectedJobDetail = null;
    }
  }

  async function loadPlaybookDetail(playbookId: string, silent = false) {
    if (!playbookId) {
      syncPlaybookDetail(null);
      return;
    }

    jobLoading = !silent;
    try {
      const detail = await fetchPlaybookDetail(playbookId);
      syncPlaybookDetail(detail);
      error = null;

      if (selectedJobId) {
        await loadJobDetail(selectedJobId, true);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the selected playbook.';
    } finally {
      jobLoading = false;
    }
  }

  async function loadJobDetail(jobId: string, silent = false) {
    if (!jobId) {
      selectedJobDetail = null;
      return;
    }

    if (!silent) {
      jobLoading = true;
    }

    try {
      selectedJobDetail = await fetchJobDetail(jobId);
      selectedJobId = jobId;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the selected job.';
    } finally {
      jobLoading = false;
    }
  }

  async function loadAll(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      const [nextOverview, nextPlaybooks] = await Promise.all([fetchOverview(), fetchPlaybooks()]);
      overview = nextOverview;
      playbooks = nextPlaybooks;
      error = null;

      const nextSelectedPlaybookId =
        nextPlaybooks.some((playbook) => playbook.id === selectedPlaybookId)
          ? selectedPlaybookId
          : (nextPlaybooks[0]?.id ?? '');
      if (nextSelectedPlaybookId) {
        await loadPlaybookDetail(nextSelectedPlaybookId, true);
      } else {
        syncPlaybookDetail(null);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load Nucleus playbooks.';
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function handleCreatePlaybook() {
    const fallbackProfile = workspace?.profiles.find((profile) => profile.is_default) ?? workspace?.profiles[0];
    creating = true;
    success = null;

    try {
      const detail = await createPlaybook({
        title: 'New playbook',
        description: 'Nucleus-owned background automation.',
        prompt: 'Inspect the workspace, decide the safest next step, and finish with a concise report.',
        profile_id: fallbackProfile?.id,
        project_id: workspaceProjects[0]?.id,
        enabled: true,
        policy_bundle: 'read_only',
        trigger_kind: 'manual'
      });
      syncPlaybookDetail(detail);
      selectedJobDetail = null;
      success = 'Playbook created.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to create the playbook.';
    } finally {
      creating = false;
    }
  }

  async function handleSavePlaybook() {
    if (!playbookDetail) {
      return;
    }

    saving = true;
    success = null;

    try {
      const detail = await updatePlaybook(playbookDetail.playbook.id, {
        title: draftTitle,
        description: draftDescription,
        prompt: draftPrompt,
        profile_id: draftProfileId || '',
        project_id: draftProjectId || '',
        enabled: draftEnabled,
        policy_bundle: draftPolicyBundle,
        trigger_kind: draftTriggerKind,
        schedule_interval_secs:
          draftTriggerKind === 'schedule'
            ? Number.parseInt(draftScheduleIntervalSecs, 10)
            : null,
        event_kind: draftTriggerKind === 'event' ? draftEventKind : null
      });
      syncPlaybookDetail(detail);
      success = 'Playbook updated.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the playbook.';
    } finally {
      saving = false;
    }
  }

  async function handleDeletePlaybook() {
    if (!playbookDetail) {
      return;
    }

    deleting = true;
    success = null;

    try {
      const deleted = await deletePlaybook(playbookDetail.playbook.id);
      playbooks = playbooks.filter((playbook) => playbook.id !== deleted.playbook.id);
      const nextPlaybookId = playbooks[0]?.id ?? '';
      if (nextPlaybookId) {
        await loadPlaybookDetail(nextPlaybookId, true);
      } else {
        syncPlaybookDetail(null);
      }
      success = 'Playbook deleted.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete the playbook.';
    } finally {
      deleting = false;
    }
  }

  async function handleRunPlaybook() {
    if (!playbookDetail) {
      return;
    }

    running = true;
    success = null;

    try {
      const job = await runPlaybook(playbookDetail.playbook.id);
      selectedJobId = job.job.id;
      selectedJobDetail = job;
      await loadPlaybookDetail(playbookDetail.playbook.id, true);
      success = 'Playbook queued.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to run the playbook.';
    } finally {
      running = false;
    }
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event === 'overview.updated') {
      overview = event.data;
      error = null;
      return;
    }

    if (
      (event.event === 'job.created' ||
        event.event === 'job.updated' ||
        event.event === 'job.completed' ||
        event.event === 'job.failed') &&
      event.data.template_id &&
      event.data.template_id === selectedPlaybookId
    ) {
      void loadAll(true);
      if (selectedJobId && event.data.id === selectedJobId) {
        void loadJobDetail(selectedJobId, true);
      }
    }
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
  <title>Nucleus - Automations</title>
  <meta
    name="description"
    content="Nucleus-owned playbooks, schedules, and event-triggered automation jobs."
  />
</svelte:head>

<div class="space-y-8">
  <section class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
    <div class="space-y-3">
      <Badge variant={error ? 'destructive' : 'default'}>{statusLabel}</Badge>
      <div>
        <h1 class="text-3xl font-semibold text-zinc-50">Automations</h1>
        <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">
          Saved playbooks run through the same Nucleus-owned Utility Worker engine as chat jobs, including
          approvals, audit, artifacts, and write-scope locking.
        </p>
      </div>
    </div>

    <div class="flex flex-wrap gap-3">
      <Button variant="outline" onclick={() => void loadAll(true)} disabled={refreshing}>
        <RefreshCw class={refreshing ? 'size-4 animate-spin' : 'size-4'} />
        {refreshing ? 'Refreshing' : 'Refresh'}
      </Button>
      <Button onclick={handleCreatePlaybook} disabled={creating}>
        <Plus class="size-4" />
        {creating ? 'Creating' : 'New playbook'}
      </Button>
    </div>
  </section>

  {#if error}
    <div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
      {error}
    </div>
  {/if}

  {#if success}
    <div class="rounded-lg border border-lime-400/20 bg-lime-400/10 px-4 py-3 text-sm text-lime-100">
      {success}
    </div>
  {/if}

  <section class="grid gap-6 xl:grid-cols-[20rem_minmax(0,1fr)]">
    <Card>
      <CardHeader>
        <CardTitle>Saved Playbooks</CardTitle>
        <CardDescription>
          Utility automation sessions stay out of the normal chat sidebar, but their jobs still use
          the same Nucleus truth.
        </CardDescription>
      </CardHeader>
      <CardContent class="space-y-3">
        {#if playbooks.length === 0}
          <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
            No playbooks yet. Create one to schedule or trigger Nucleus-owned work.
          </div>
        {:else}
          {#each playbooks as playbook}
            <button
              type="button"
              class={`w-full rounded-xl border px-4 py-3 text-left transition ${
                selectedPlaybookId === playbook.id
                  ? 'border-lime-400/40 bg-lime-400/10'
                  : 'border-zinc-800 bg-zinc-950/60 hover:border-zinc-700'
              }`}
              onclick={() => void loadPlaybookDetail(playbook.id)}
            >
              <div class="flex items-start justify-between gap-3">
                <div>
                  <div class="text-sm font-medium text-zinc-100">{playbook.title}</div>
                  <div class="mt-1 text-xs text-zinc-500">{playbook.prompt_excerpt}</div>
                </div>
                <Badge variant={playbook.enabled ? 'default' : 'secondary'}>
                  {playbook.enabled ? 'Enabled' : 'Disabled'}
                </Badge>
              </div>
              <div class="mt-3 flex flex-wrap gap-2 text-[11px] text-zinc-500">
                <span>{formatState(playbook.trigger_kind)}</span>
                <span>{formatState(playbook.policy_bundle)}</span>
                <span>{compactPath(playbook.working_dir)}</span>
                {#if playbook.last_run_at}
                  <span>Last run {formatDateTime(playbook.last_run_at)}</span>
                {/if}
              </div>
            </button>
          {/each}
        {/if}
      </CardContent>
    </Card>

    <div class="space-y-6">
      {#if !playbookDetail}
        <Card>
          <CardHeader>
            <CardTitle>Select A Playbook</CardTitle>
            <CardDescription>
              Pick an existing playbook or create a new one to configure automation triggers and
              policy bundles.
            </CardDescription>
          </CardHeader>
        </Card>
      {:else}
        <Card>
          <CardHeader>
            <div class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
              <div class="space-y-2">
                <CardTitle>{playbookDetail.playbook.title}</CardTitle>
                <CardDescription>
                  Hidden session: {compactPath(playbookDetail.session.working_dir)} ·
                  {playbookDetail.playbook.project_title || ' Workspace scratch'}
                </CardDescription>
              </div>

              <div class="flex flex-wrap gap-3">
                <Button variant="outline" onclick={handleRunPlaybook} disabled={running}>
                  <Play class="size-4" />
                  {running ? 'Queueing' : 'Run now'}
                </Button>
                <Button onclick={handleSavePlaybook} disabled={!draftDirty || saving}>
                  <Save class="size-4" />
                  {saving ? 'Saving' : 'Save'}
                </Button>
                <Button variant="outline" onclick={handleDeletePlaybook} disabled={deleting}>
                  <Trash2 class="size-4" />
                  {deleting ? 'Deleting' : 'Delete'}
                </Button>
              </div>
            </div>
          </CardHeader>
          <CardContent class="grid gap-6 lg:grid-cols-2">
            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Title</span>
              <input
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftTitle}
              />
            </label>

            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Description</span>
              <input
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftDescription}
              />
            </label>

            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Workspace profile</span>
              <select
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftProfileId}
              >
                {#each workspaceProfiles as profile}
                  <option value={profile.id}>{profile.title}</option>
                {/each}
              </select>
            </label>

            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Project scope</span>
              <select
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftProjectId}
              >
                <option value="">Workspace scratch</option>
                {#each workspaceProjects as project}
                  <option value={project.id}>{project.title}</option>
                {/each}
              </select>
            </label>

            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Policy bundle</span>
              <select
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftPolicyBundle}
              >
                {#each policyBundles as bundle}
                  <option value={bundle.value}>{bundle.label}</option>
                {/each}
              </select>
              <div class="text-xs leading-5 text-zinc-500">{selectedBundle.summary}</div>
            </label>

            <label class="space-y-2 text-sm">
              <span class="font-medium text-zinc-200">Trigger</span>
              <select
                class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftTriggerKind}
              >
                {#each triggerOptions as trigger}
                  <option value={trigger.value}>{trigger.label}</option>
                {/each}
              </select>
            </label>

            {#if draftTriggerKind === 'schedule'}
              <label class="space-y-2 text-sm">
                <span class="font-medium text-zinc-200">Schedule interval (seconds)</span>
                <input
                  class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                  bind:value={draftScheduleIntervalSecs}
                  inputmode="numeric"
                />
              </label>
            {/if}

            {#if draftTriggerKind === 'event'}
              <label class="space-y-2 text-sm">
                <span class="font-medium text-zinc-200">Event source</span>
                <select
                  class="w-full rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
                  bind:value={draftEventKind}
                >
                  {#each eventOptions as eventOption}
                    <option value={eventOption.value}>{eventOption.label}</option>
                  {/each}
                </select>
              </label>
            {/if}

            <label class="flex items-center gap-3 rounded-lg border border-zinc-800 bg-zinc-950 px-3 py-3 text-sm text-zinc-200">
              <input type="checkbox" bind:checked={draftEnabled} class="size-4 accent-lime-400" />
              <span>Allow this playbook to auto-trigger.</span>
            </label>

            <div class="rounded-xl border border-zinc-800 bg-zinc-950/70 px-4 py-3 text-sm text-zinc-400">
              <div class="flex items-center gap-2 text-zinc-200">
                <Workflow class="size-4" />
                Utility Worker target
              </div>
              <div class="mt-2 text-xs leading-5 text-zinc-500">
                Visible route: {playbookDetail.session.provider} / {playbookDetail.session.model}
                <br />
                Utility profile: {selectedProfile?.utility.adapter ?? 'unknown'} /
                {selectedProfile?.utility.model || 'default'}
              </div>
            </div>

            <label class="space-y-2 text-sm lg:col-span-2">
              <span class="font-medium text-zinc-200">Prompt</span>
              <textarea
                class="min-h-[15rem] w-full rounded-xl border border-zinc-800 bg-zinc-950 px-3 py-3 text-sm leading-6 text-zinc-100 outline-none transition focus:border-lime-400/40"
                bind:value={draftPrompt}
              ></textarea>
            </label>
          </CardContent>
        </Card>

        <section class="grid gap-6 xl:grid-cols-[24rem_minmax(0,1fr)]">
          <Card>
            <CardHeader>
              <CardTitle>Recent Jobs</CardTitle>
              <CardDescription>
                Triggered runs reuse the same approval, artifact, and audit contracts as chat jobs.
              </CardDescription>
            </CardHeader>
            <CardContent class="space-y-3">
              {#if playbookDetail.recent_jobs.length === 0}
                <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
                  No playbook jobs have been queued yet.
                </div>
              {:else}
                {#each playbookDetail.recent_jobs as job}
                  <button
                    type="button"
                    class={`w-full rounded-xl border px-4 py-3 text-left transition ${
                      selectedJobId === job.id
                        ? 'border-lime-400/40 bg-lime-400/10'
                        : 'border-zinc-800 bg-zinc-950/60 hover:border-zinc-700'
                    }`}
                    onclick={() => void loadJobDetail(job.id)}
                  >
                    <div class="flex items-start justify-between gap-3">
                      <div>
                        <div class="text-sm font-medium text-zinc-100">{job.title}</div>
                        <div class="mt-1 text-xs text-zinc-500">{job.prompt_excerpt}</div>
                      </div>
                      <Badge variant={badgeVariantForJobState(job.state)}>
                        {formatState(job.state)}
                      </Badge>
                    </div>
                    <div class="mt-3 flex flex-wrap gap-2 text-[11px] text-zinc-500">
                      <span>{formatState(job.trigger_kind)}</span>
                      <span>{job.pending_approval_count} approvals</span>
                      <span>{job.artifact_count} artifacts</span>
                      <span>{formatDateTime(job.updated_at)}</span>
                    </div>
                  </button>
                {/each}
              {/if}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>{selectedRecentJob ? selectedRecentJob.title : 'Job Detail'}</CardTitle>
              <CardDescription>
                Timeline and outputs for the selected Nucleus-owned automation job.
              </CardDescription>
            </CardHeader>
            <CardContent class="space-y-5">
              {#if jobLoading}
                <div class="text-sm text-zinc-500">Loading job detail…</div>
              {:else if !selectedJobDetail}
                <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
                  Select a playbook job to inspect its timeline, approvals, and artifacts.
                </div>
              {:else}
                <div class="grid gap-3 md:grid-cols-3">
                  <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                    <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">State</div>
                    <div class="mt-2">
                      <Badge variant={badgeVariantForJobState(selectedJobDetail.job.state)}>
                        {formatState(selectedJobDetail.job.state)}
                      </Badge>
                    </div>
                  </div>
                  <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                    <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Approvals</div>
                    <div class="mt-2 text-lg font-semibold text-zinc-100">
                      {selectedJobDetail.job.pending_approval_count}
                    </div>
                  </div>
                  <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                    <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Artifacts</div>
                    <div class="mt-2 text-lg font-semibold text-zinc-100">
                      {selectedJobDetail.job.artifact_count}
                    </div>
                  </div>
                </div>

                <div class="space-y-3">
                  <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
                    <TimerReset class="size-4" />
                    Timeline
                  </div>
                  {#if selectedJobDetail.events.length === 0}
                    <div class="text-sm text-zinc-500">No job events recorded yet.</div>
                  {:else}
                    <div class="space-y-3">
                      {#each [...selectedJobDetail.events].reverse().slice(0, 8) as event}
                        <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                          <div class="flex items-start justify-between gap-3">
                            <div>
                              <div class="text-sm font-medium text-zinc-100">{event.summary}</div>
                              <div class="mt-1 text-xs leading-5 text-zinc-500">{event.detail}</div>
                            </div>
                            <Badge variant={badgeVariantForJobState(event.status)}>
                              {formatState(event.status)}
                            </Badge>
                          </div>
                          <div class="mt-2 text-[11px] text-zinc-600">
                            {event.event_type} · {formatDateTime(event.created_at)}
                          </div>
                        </div>
                      {/each}
                    </div>
                  {/if}
                </div>

                <div class="space-y-3">
                  <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
                    <CalendarClock class="size-4" />
                    Approvals
                  </div>
                  {#if selectedJobDetail.approvals.length === 0}
                    <div class="text-sm text-zinc-500">No approvals were recorded for this job.</div>
                  {:else}
                    <div class="space-y-3">
                      {#each [...selectedJobDetail.approvals].reverse().slice(0, 4) as approval}
                        <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                          <div class="flex items-start justify-between gap-3">
                            <div>
                              <div class="text-sm font-medium text-zinc-100">{approval.summary}</div>
                              <div class="mt-1 text-xs leading-5 text-zinc-500">{approval.detail}</div>
                            </div>
                            <Badge variant={badgeVariantForJobState(approval.state)}>
                              {formatState(approval.state)}
                            </Badge>
                          </div>
                          {#if approval.diff_preview}
                            <pre class="mt-3 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{approval.diff_preview}</pre>
                          {/if}
                        </div>
                      {/each}
                    </div>
                  {/if}
                </div>

                <div class="space-y-3">
                  <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
                    <Play class="size-4" />
                    Artifacts
                  </div>
                  {#if selectedJobDetail.artifacts.length === 0}
                    <div class="text-sm text-zinc-500">No artifacts were recorded for this job.</div>
                  {:else}
                    <div class="space-y-3">
                      {#each [...selectedJobDetail.artifacts].reverse().slice(0, 4) as artifact}
                        <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                          <div class="flex items-start justify-between gap-3">
                            <div>
                              <div class="text-sm font-medium text-zinc-100">{artifact.title}</div>
                              <div class="mt-1 text-xs text-zinc-500">
                                {artifact.kind} · {formatDateTime(artifact.created_at)}
                              </div>
                            </div>
                            <div class="text-[11px] text-zinc-600">{artifact.size_bytes} bytes</div>
                          </div>
                          {#if artifact.preview_text}
                            <pre class="mt-3 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{artifact.preview_text}</pre>
                          {/if}
                          <div class="mt-2 text-[11px] text-zinc-600">{compactPath(artifact.path)}</div>
                        </div>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </CardContent>
          </Card>
        </section>
      {/if}
    </div>
  </section>
</div>
