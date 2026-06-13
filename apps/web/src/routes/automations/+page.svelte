<script lang="ts">
  import { onMount } from 'svelte';
  import {
    CalendarClock,
    ChevronDown,
    Cog,
    Play,
    Plus,
    Power,
    PowerOff,
    RefreshCw,
    Save,
    ScrollText,
    TimerReset,
    Trash2,
    Workflow
  } from 'lucide-svelte';

  import FriendlyErrorNotice from '$lib/components/app/session/friendly-error-notice.svelte';
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
    disableLocalJob,
    enableLocalJob,
    fetchJobDetail,
    fetchLocalJobDetail,
    fetchLocalJobs,
    fetchOverview,
    fetchPlaybookDetail,
    fetchPlaybooks,
    runLocalJob,
    runPlaybook,
    updatePlaybook
  } from '$lib/nucleus/client';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import { localJobBadgeVariant, localJobCanToggle } from '$lib/nucleus/local-jobs';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    DaemonEvent,
    JobDetail,
    JobSummary,
    LocalJobDetail,
    LocalJobSummary,
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
  let localJobs = $state<LocalJobSummary[]>([]);
  let playbookDetail = $state<PlaybookDetail | null>(null);
  let localJobDetail = $state<LocalJobDetail | null>(null);
  let activeSurface = $state<'playbooks' | 'local_jobs'>('playbooks');
  let selectedPlaybookId = $state('');
  let selectedLocalJobUnit = $state('');
  let selectedJobId = $state('');
  let selectedJobDetail = $state<JobDetail | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let saving = $state(false);
  let creating = $state(false);
  let deleting = $state(false);
  let running = $state(false);
  let jobLoading = $state(false);
  let localJobLoading = $state(false);
  let localJobAction = $state<string | null>(null);
  let localJobLoadError = $state<string | null>(null);
  let expandedLogUnit = $state<string | null>(null);
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
  let selectedLocalJob = $derived(
    localJobDetail?.summary ??
      localJobs.find((job) => job.unit === selectedLocalJobUnit) ??
      localJobs[0] ??
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

  function formatOptionalDate(value: number | null): string {
    return value ? formatDateTime(value) : 'Not recorded';
  }

  function formatLocalJobExit(job: LocalJobSummary): string {
    const code = job.last_exit.code === null ? 'no code' : `exit ${job.last_exit.code}`;
    return `${job.last_exit.result || 'unknown'} · ${code}`;
  }

  function localJobActionKey(unit: string, action: string): string {
    return `${unit}:${action}`;
  }

  function syncLocalJobSummary(next: LocalJobSummary) {
    const remaining = localJobs.filter((job) => job.unit !== next.unit);
    localJobs = [next, ...remaining].sort((left, right) => left.unit.localeCompare(right.unit));
    if (localJobDetail?.summary.unit === next.unit) {
      localJobDetail = { ...localJobDetail, summary: next };
    }
  }

  function syncLocalJobs(next: LocalJobSummary[]) {
    localJobs = [...next].sort((left, right) => left.unit.localeCompare(right.unit));
    if (!localJobs.some((job) => job.unit === selectedLocalJobUnit)) {
      selectedLocalJobUnit = localJobs[0]?.unit ?? '';
      localJobDetail = null;
    }
    if (localJobDetail && !localJobs.some((job) => job.unit === localJobDetail?.summary.unit)) {
      localJobDetail = null;
    }
  }

  function jobCompletionLabel(job: JobSummary): string {
    if (job.state !== 'completed' || !job.browser_verification_required) {
      return formatState(job.state);
    }
    if (job.browser_verification_status === 'passed') return 'Completed, browser-verified';
    if (job.browser_verification_status === 'failed') {
      return 'Completed, browser verification failed';
    }
    if (job.browser_verification_status === 'unavailable') {
      return 'Completed, verification unavailable';
    }
    return 'Completed, not browser-verified';
  }

  function formatVerificationStatus(status: string): string {
    if (status === 'passed') return 'Browser-verified';
    if (status === 'failed') return 'Browser verification failed';
    if (status === 'unavailable') return 'Verification unavailable';
    if (status === 'not_performed') return 'Not browser-verified';
    if (status === 'pending') return 'Verification pending';
    return 'Not required';
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

  async function loadLocalJobDetail(unit: string, silent = false) {
    if (!unit) {
      localJobDetail = null;
      return;
    }

    selectedLocalJobUnit = unit;
    if (localJobDetail?.summary.unit !== unit) {
      localJobDetail = null;
    }

    if (!silent) {
      localJobLoading = true;
    }

    try {
      const detail = await fetchLocalJobDetail(unit);
      localJobDetail = detail;
      expandedLogUnit = unit;
      syncLocalJobSummary(detail.summary);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the selected local job.';
    } finally {
      localJobLoading = false;
    }
  }

  async function handleLocalJobAction(unit: string, action: 'enable' | 'disable' | 'run') {
    localJobAction = localJobActionKey(unit, action);
    success = null;

    try {
      const next =
        action === 'enable'
          ? await enableLocalJob(unit)
          : action === 'disable'
            ? await disableLocalJob(unit)
            : await runLocalJob(unit);
      syncLocalJobSummary(next);
      selectedLocalJobUnit = next.unit;
      if (expandedLogUnit === next.unit || action === 'run') {
        await loadLocalJobDetail(next.unit, true);
      }
      success =
        action === 'enable'
          ? 'Local job enabled.'
          : action === 'disable'
            ? 'Local job disabled.'
            : 'Local job handed off to systemd.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the local job.';
    } finally {
      localJobAction = null;
    }
  }

  async function loadAll(silent = false) {
    if (!silent) {
      loading = overview === null;
    }

    refreshing = silent;

    try {
      const [nextOverview, nextPlaybooks] = await Promise.all([
        fetchOverview(),
        fetchPlaybooks()
      ]);
      overview = nextOverview;
      playbooks = nextPlaybooks;
      error = null;

      try {
        syncLocalJobs(await fetchLocalJobs());
        localJobLoadError = null;
      } catch (cause) {
        localJobLoadError = cause instanceof Error ? cause.message : 'Failed to load local jobs.';
      }

      const nextSelectedPlaybookId =
        nextPlaybooks.some((playbook) => playbook.id === selectedPlaybookId)
          ? selectedPlaybookId
          : (nextPlaybooks[0]?.id ?? '');
      if (nextSelectedPlaybookId) {
        await loadPlaybookDetail(nextSelectedPlaybookId, true);
      } else {
        syncPlaybookDetail(null);
      }

      if (activeSurface === 'local_jobs' && selectedLocalJobUnit) {
        await loadLocalJobDetail(selectedLocalJobUnit, true);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load automations.';
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

    if (event.event === 'local_jobs.updated') {
      syncLocalJobs(event.data);
      localJobLoadError = null;
      if (localJobDetail) {
        const nextSummary = event.data.find((job) => job.unit === localJobDetail?.summary.unit);
        if (nextSummary) {
          localJobDetail = { ...localJobDetail, summary: nextSummary };
        }
      }
      error = null;
      return;
    }

    if (
      (event.event === 'job.created' ||
        event.event === 'job.updated' ||
        event.event === 'job.completed' ||
        event.event === 'job.blocked' ||
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
          {activeSurface === 'playbooks'
            ? 'Saved playbooks run through the same Nucleus-owned Utility Worker engine as chat jobs, including approvals, audit, artifacts, and write-scope locking.'
            : 'OS-scheduled. Nucleus observes and controls these units — it never runs them.'}
        </p>
      </div>
    </div>

    <div class="flex flex-wrap gap-3">
      <Button variant="outline" onclick={() => void loadAll(true)} disabled={refreshing}>
        <RefreshCw class={refreshing ? 'size-4 animate-spin' : 'size-4'} />
        {refreshing ? 'Refreshing' : 'Refresh'}
      </Button>
      {#if activeSurface === 'playbooks'}
        <Button onclick={handleCreatePlaybook} disabled={creating}>
          <Plus class="size-4" />
          {creating ? 'Creating' : 'New playbook'}
        </Button>
      {/if}
    </div>
  </section>

  <div class="inline-flex rounded-lg border border-zinc-800 bg-zinc-950 p-1 text-sm">
    <button
      type="button"
      class={`rounded-md px-3 py-2 transition ${
        activeSurface === 'playbooks' ? 'bg-zinc-800 text-zinc-50' : 'text-zinc-400 hover:text-zinc-100'
      }`}
      onclick={() => {
        activeSurface = 'playbooks';
      }}
    >
      Playbooks
    </button>
    <button
      type="button"
      class={`rounded-md px-3 py-2 transition ${
        activeSurface === 'local_jobs' ? 'bg-zinc-800 text-zinc-50' : 'text-zinc-400 hover:text-zinc-100'
      }`}
      onclick={() => {
        activeSurface = 'local_jobs';
        if (selectedLocalJobUnit && !localJobDetail) {
          void loadLocalJobDetail(selectedLocalJobUnit, true);
        }
      }}
    >
      Local jobs
    </button>
  </div>

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

  {#if activeSurface === 'playbooks'}
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
                        {jobCompletionLabel(job)}
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
                        {jobCompletionLabel(selectedJobDetail.job)}
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

                {#if selectedJobDetail.job.browser_verification_required || selectedJobDetail.job.browser_verification_status !== 'not_required'}
                  <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
                    <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Browser Verification</div>
                    <div class="mt-2 text-sm font-medium text-zinc-100">
                      {formatVerificationStatus(selectedJobDetail.job.browser_verification_status)}
                    </div>
                    {#if selectedJobDetail.job.browser_verification_summary}
                      <div class="mt-1 text-xs leading-5 text-zinc-500">
                        {selectedJobDetail.job.browser_verification_summary}
                      </div>
                    {/if}
                    {#if selectedJobDetail.job.browser_verification_artifact_ids.length > 0}
                      <div class="mt-3 flex flex-wrap gap-1.5">
                        {#each selectedJobDetail.job.browser_verification_artifact_ids as artifactId}
                          <span class="rounded border border-zinc-800 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-500">{artifactId}</span>
                        {/each}
                      </div>
                    {/if}
                  </div>
                {/if}

                {#if selectedJobDetail.job.user_error}
                  <FriendlyErrorNotice userError={selectedJobDetail.job.user_error} />
                {:else if selectedJobDetail.job.last_error}
                  <div class="rounded-lg border border-red-500/25 bg-red-500/10 px-4 py-3 text-sm text-red-200">
                    {selectedJobDetail.job.last_error}
                  </div>
                {/if}

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
  {:else}
  <section class="space-y-6">
    <div class="flex items-center gap-2 text-sm text-zinc-400">
      <Cog class="size-4 text-zinc-300" />
      <span>OS-scheduled. Nucleus observes and controls these units — it never runs them.</span>
    </div>

    {#if localJobLoadError}
      <div class="rounded-lg border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
        {localJobLoadError}
      </div>
    {/if}

    {#if localJobs.length === 0}
      <Card>
        <CardHeader>
          <CardTitle>Local Jobs</CardTitle>
          <CardDescription>
            No systemd user timers are configured for this workspace allowlist.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm leading-6 text-zinc-500">
            Add unit globs to the daemon setting <span class="font-mono text-zinc-300">system_jobs_unit_globs</span>
            to observe existing OS-scheduled user timers here.
          </div>
        </CardContent>
      </Card>
    {:else}
      <section class="grid gap-6 xl:grid-cols-[minmax(0,1fr)_28rem]">
        <div class="space-y-3">
          {#each localJobs as job}
            <div
              class={`rounded-xl border px-4 py-4 transition ${
                selectedLocalJobUnit === job.unit
                  ? 'border-lime-400/40 bg-lime-400/10'
                  : 'border-zinc-800 bg-zinc-950/60'
              }`}
            >
              <div class="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
                <button
                  type="button"
                  class="min-w-0 text-left"
                  onclick={() => void loadLocalJobDetail(job.unit)}
                >
                  <div class="flex flex-wrap items-center gap-2">
                    <Cog class="size-4 text-zinc-400" />
                    <span class="text-sm font-medium text-zinc-100">{job.title}</span>
                    <Badge variant="secondary">systemd · user</Badge>
                    <Badge variant={job.enabled ? 'default' : 'secondary'}>
                      {job.enabled ? 'enabled' : 'disabled'}
                    </Badge>
                    <Badge variant="secondary">{job.unit_file_state}</Badge>
                    <Badge variant={localJobBadgeVariant(job)}>{job.active_state}</Badge>
                  </div>
                  <div class="mt-2 break-all font-mono text-xs text-zinc-500">{job.unit}</div>
                </button>

                <div class="flex flex-wrap gap-2">
                  {#if localJobCanToggle(job) && job.enabled}
                    <Button
                      variant="outline"
                      onclick={() => void handleLocalJobAction(job.unit, 'disable')}
                      disabled={localJobAction !== null}
                    >
                      <PowerOff class="size-4" />
                      {localJobAction === localJobActionKey(job.unit, 'disable') ? 'Disabling' : 'Disable'}
                    </Button>
                  {:else if localJobCanToggle(job)}
                    <Button
                      variant="outline"
                      onclick={() => void handleLocalJobAction(job.unit, 'enable')}
                      disabled={localJobAction !== null}
                    >
                      <Power class="size-4" />
                      {localJobAction === localJobActionKey(job.unit, 'enable') ? 'Enabling' : 'Enable'}
                    </Button>
                  {/if}
                  <Button
                    onclick={() => void handleLocalJobAction(job.unit, 'run')}
                    disabled={localJobAction !== null}
                  >
                    <Play class="size-4" />
                    {localJobAction === localJobActionKey(job.unit, 'run') ? 'Starting' : 'Run now'}
                  </Button>
                </div>
              </div>

              <div class="mt-4 grid gap-3 md:grid-cols-4">
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Next elapse</div>
                  <div class="mt-2 text-sm text-zinc-100">{formatOptionalDate(job.schedule.next_elapse_at)}</div>
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Last fired</div>
                  <div class="mt-2 text-sm text-zinc-100">{formatOptionalDate(job.last_fired_at)}</div>
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Last exit</div>
                  <div class="mt-2 text-sm text-zinc-100">{formatLocalJobExit(job)}</div>
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Triggers</div>
                  <div class="mt-2 break-all font-mono text-xs text-zinc-100">{job.triggered_unit}</div>
                </div>
              </div>
            </div>
          {/each}
        </div>

        <Card>
          <CardHeader>
            <CardTitle>{selectedLocalJob ? selectedLocalJob.title : 'Local Job Detail'}</CardTitle>
            <CardDescription>
              {selectedLocalJob ? selectedLocalJob.unit : 'Select a systemd user timer to inspect its service log tail.'}
            </CardDescription>
          </CardHeader>
          <CardContent class="space-y-5">
            {#if localJobLoading}
              <div class="text-sm text-zinc-500">Loading local job detail…</div>
            {:else if !selectedLocalJob}
              <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
                Select a local job to inspect its state, service handoff, and journal tail.
              </div>
            {:else}
              <div class="grid gap-3 sm:grid-cols-2">
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Backend</div>
                  <div class="mt-2 text-sm text-zinc-100">systemd · user</div>
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Active state</div>
                  <div class="mt-2">
                    <Badge variant={localJobBadgeVariant(selectedLocalJob)}>{selectedLocalJob.active_state}</Badge>
                  </div>
                  <div class="mt-1 text-xs text-zinc-500">{selectedLocalJob.unit_file_state}</div>
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Next elapse</div>
                  <div class="mt-2 text-sm text-zinc-100">{formatOptionalDate(selectedLocalJob.schedule.next_elapse_at)}</div>
                  {#if selectedLocalJob.schedule.raw}
                    <div class="mt-1 text-xs text-zinc-500">{selectedLocalJob.schedule.raw}</div>
                  {/if}
                </div>
                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.18em] text-zinc-500">Last exit</div>
                  <div class="mt-2 text-sm text-zinc-100">{formatLocalJobExit(selectedLocalJob)}</div>
                  <div class="mt-1 text-xs text-zinc-500">{formatOptionalDate(selectedLocalJob.last_exit.at)}</div>
                </div>
              </div>

              <div class="flex flex-wrap gap-2">
                {#if localJobCanToggle(selectedLocalJob) && selectedLocalJob.enabled}
                  <Button
                    variant="outline"
                    onclick={() => void handleLocalJobAction(selectedLocalJob.unit, 'disable')}
                    disabled={localJobAction !== null}
                  >
                    <PowerOff class="size-4" />
                    Disable
                  </Button>
                {:else if localJobCanToggle(selectedLocalJob)}
                  <Button
                    variant="outline"
                    onclick={() => void handleLocalJobAction(selectedLocalJob.unit, 'enable')}
                    disabled={localJobAction !== null}
                  >
                    <Power class="size-4" />
                    Enable
                  </Button>
                {/if}
                <Button
                  onclick={() => void handleLocalJobAction(selectedLocalJob.unit, 'run')}
                  disabled={localJobAction !== null}
                >
                  <Play class="size-4" />
                  Run now
                </Button>
                <Button
                  variant="outline"
                  onclick={() => {
                    expandedLogUnit = expandedLogUnit === selectedLocalJob.unit ? null : selectedLocalJob.unit;
                    if (expandedLogUnit === selectedLocalJob.unit && !localJobDetail) {
                      void loadLocalJobDetail(selectedLocalJob.unit, true);
                    }
                  }}
                >
                  {#if expandedLogUnit === selectedLocalJob.unit}
                    <ChevronDown class="size-4" />
                  {:else}
                    <ScrollText class="size-4" />
                  {/if}
                  Log tail
                </Button>
              </div>

              {#if expandedLogUnit === selectedLocalJob.unit}
                <div class="space-y-3">
                  <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
                    <ScrollText class="size-4" />
                    Journal tail
                  </div>
                  {#if !localJobDetail}
                    <div class="text-sm text-zinc-500">Select this job to load its journal tail.</div>
                  {:else if localJobDetail.log_tail.length === 0}
                    <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
                      No journal lines were returned for the triggered service.
                    </div>
                  {:else}
                    <pre class="max-h-[28rem] overflow-auto rounded-xl border border-zinc-800 bg-zinc-950 px-3 py-3 text-xs leading-5 text-zinc-400">{localJobDetail.log_tail.join('\n')}</pre>
                  {/if}
                </div>
              {/if}
            {/if}
          </CardContent>
        </Card>
      </section>
    {/if}
  </section>
  {/if}
</div>
