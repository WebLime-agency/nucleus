<script lang="ts">
  import { browser } from '$app/environment';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount, tick } from 'svelte';
  import {
    Archive,
    Bot,
    ArrowLeft,
    ArrowRight,
    Compass,
    ChevronDown,
    ChevronUp,
    Clock3,
    FolderTree,
    ImagePlus,
    MessageSquare,
    MonitorSmartphone,
    NotebookPen,
    PanelRightOpen,
    Plus,
    RotateCcw,
    Router,
    Save,
    Send,
    Trash2,
    Wrench,
    Workflow,
    X,
    XCircle
  } from 'lucide-svelte';

  import FriendlyErrorNotice from '$lib/components/app/session/friendly-error-notice.svelte';
  import MarkdownContent from '$lib/components/session/markdown-content.svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
  import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '$lib/components/ui/sheet';
  import { Textarea } from '$lib/components/ui/textarea';
  import {
    approveRequest,
    cancelJob,
    createSession,
    deleteSession,
    denyRequest,
    captureBrowserSnapshot,
    fetchActions,
    fetchBrowserContext,
    fetchAuditEvents,
    fetchJobDetail,
    fetchOverview,
    fetchSessionJobs,
    fetchSessionDetail,
    resumeJob,
    runAction,
    sendSessionPrompt,
    navigateBrowser,
    openBrowserTab,
    requestBrowserAnnotation,
    selectBrowserPage,
    sendBrowserAction,
    sendBrowserCommand,
    startBrowserStream,
    stopBrowserStream,
    updateSession
  } from '$lib/nucleus/client';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    ActionSummary,
    ApprovalRequestSummary,
    ArtifactSummary,
    BrowserContextSummary,
    BrowserSnapshot,
    AuditEvent,
    CommandSessionSummary,
    DaemonEvent,
    JobDetail,
    JobEvent,
    JobSummary,
    PromptProgressUpdate,
    RuntimeOverview,
    SessionDetail,
    SessionSummary,
    SessionTurn,
    SessionTurnImage,
    ToolCallSummary,
    WorkerSummary
  } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';

  const DEFAULT_AUDIT_LIMIT = 12;
  const MAX_IMAGES = 5;
  const MAX_IMAGE_SIZE_BYTES = 10 * 1024 * 1024;
  const MAX_TOTAL_IMAGE_SIZE_BYTES = 50 * 1024 * 1024;

  type ComposerImage = SessionTurnImage & {
    id: string;
    size_bytes: number;
  };

  type SessionComposerMode = 'plan' | 'ask' | 'trusted';
  type SessionRunBudgetMode = 'inherit' | 'standard' | 'extended' | 'marathon' | 'unbounded';
  type SessionDrawerMode = 'details' | 'browser';
  type BrowserViewportMode = 'fit' | 'mobile' | 'desktop' | 'wide';
  type BrowserAnnotationDraft = { page_id: string; x: number; y: number };

  const COMPOSER_MODES: SessionComposerMode[] = ['plan', 'ask', 'trusted'];
  const RUN_BUDGET_MODES: SessionRunBudgetMode[] = [
    'inherit',
    'standard',
    'extended',
    'marathon',
    'unbounded'
  ];
  const BROWSER_VIEWPORT_MODES: BrowserViewportMode[] = ['fit', 'mobile', 'desktop', 'wide'];

  let overview = $state<RuntimeOverview | null>(null);
  let actions = $state<ActionSummary[]>([]);
  let auditEvents = $state<AuditEvent[]>([]);
  let detail = $state<SessionDetail | null>(null);
  let selectedSessionId = $state('');
  let loading = $state(true);
  let sessionLoading = $state(false);
  let sessionRequestInFlight = $state('');
  let savingSession = $state(false);
  let sending = $state(false);
  let actioning = $state(false);
  let actionRunningId = $state<string | null>(null);
  let actionConfirmId = $state<string | null>(null);
  let deleteConfirmId = $state<string | null>(null);
  let updatingProjectId = $state<string | null>(null);
  let error = $state<string | null>(null);
  let actionResultMessage = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');
  let promptText = $state('');
  let draftTitle = $state('');
  let draftProfileId = $state('');
  let draftApprovalMode = $state<'ask' | 'trusted'>('ask');
  let draftExecutionMode = $state<'act' | 'plan'>('act');
  let draftRunBudgetMode = $state<SessionRunBudgetMode>('inherit');
  let jobSummaries = $state<JobSummary[]>([]);
  let jobDetail = $state<JobDetail | null>(null);
  let selectedJobId = $state('');
  let jobLoading = $state(false);
  let jobActioning = $state(false);
  let approvalActioningId = $state<string | null>(null);
  let actionFormValues = $state<Record<string, Record<string, string>>>({});
  let sideDrawerOpen = $state(false);
  let sideDrawerMode = $state<SessionDrawerMode>('details');
  let detailPanelOpen = $derived(sideDrawerOpen && sideDrawerMode === 'details');
  let browserPanelOpen = $derived(sideDrawerOpen && sideDrawerMode === 'browser');
  let browserUrl = $state('');
  let browserUrlEditing = $state(false);
  let browserLoading = $state(false);
  let browserError = $state<string | null>(null);
  let browserContext = $state<BrowserContextSummary | null>(null);
  let browserSnapshot = $state<BrowserSnapshot | null>(null);
  let browserViewportElement = $state<HTMLImageElement | null>(null);
  let browserStageElement = $state<HTMLDivElement | null>(null);
  let browserFrameTicking = false;
  let browserStreamPageId = $state('');
  let browserStreamStarting = false;
  let browserAnnotating = $state(false);
  let browserAnnotation = $state<unknown>(null);
  let browserAnnotationDraft = $state<BrowserAnnotationDraft | null>(null);
  let browserAnnotationComment = $state('');
  let browserAnnotationSaving = $state(false);
  let browserStarting = $state(false);
  let browserTabChanging = $state(false);
  let browserViewportMode = $state<BrowserViewportMode>('fit');
  let browserReadyRequestKey = '';
  let browserInputQueue: Promise<void> = Promise.resolve();
  let browserPointerMoveFrame = 0;
  let browserPendingPointerMove: string | null = null;
  let dragOver = $state(false);
  let promptImages = $state<ComposerImage[]>([]);
  let promptProgress = $state<PromptProgressUpdate[]>([]);
  let composerActivityExpanded = $state(false);
  let composerModeMenuOpen = $state(false);
  let runBudgetMenuOpen = $state(false);
  let activityJobDetail = $state<JobDetail | null>(null);
  let activityJobRequestInFlight = $state('');
  let transcriptAnchor = $state('');

  let transcriptElement = $state<HTMLDivElement | null>(null);
  let composerTextareaElement = $state<HTMLTextAreaElement | null>(null);
  let fileInputElement = $state<HTMLInputElement | null>(null);

  let sessions = $derived(overview?.sessions ?? []);
  let routerProfiles = $derived(overview?.router_profiles ?? []);
  let workspace = $derived(overview?.workspace ?? null);
  let workspaceProjects = $derived(workspace?.projects ?? []);
  let requestedSessionId = $derived.by(() =>
    browser ? page.url.searchParams.get('session') ?? '' : ''
  );
  let selectedSession = $derived(
    detail?.session ?? sessions.find((session) => session.id === selectedSessionId) ?? null
  );
  let selectedRoute = $derived(
    routerProfiles.find((profile) => profile.id === selectedSession?.route_id) ?? null
  );
  let workspaceProfiles = $derived(workspace?.profiles ?? []);
  let selectedProfile = $derived(
    workspaceProfiles.find((profile) => profile.id === (draftProfileId || selectedSession?.profile_id || '')) ??
      null
  );
  let selectedJobSummary = $derived(
    jobSummaries.find((job) => job.id === selectedJobId) ?? jobSummaries[0] ?? null
  );
  let selectedJobHasPendingApprovals = $derived(
    jobDetail?.approvals.some((approval) => approval.state === 'pending') ?? false
  );
  let selectedSessionUserError = $derived(selectedSession?.user_error ?? null);
  let attachedProjects = $derived(selectedSession?.projects ?? []);
  let selectedProject = $derived(attachedProjects.find((project) => project.is_primary) ?? null);
  let selectedProjectTitle = $derived(
    selectedProject?.title ??
      selectedSession?.project_title ??
      (selectedSession?.project_count === 0 ? 'Workspace scratch' : 'No primary project')
  );
  let sessionSettingsDirty = $derived(
    selectedSession
      ? draftTitle !== selectedSession.title ||
          draftProfileId !== selectedSession.profile_id ||
          draftApprovalMode !== normalizeApprovalMode(selectedSession.approval_mode) ||
          draftExecutionMode !== normalizeExecutionMode(selectedSession.execution_mode) ||
          draftRunBudgetMode !== normalizeRunBudgetMode(selectedSession.run_budget_mode)
      : false
  );
  let promptReady = $derived(promptText.trim().length > 0 || promptImages.length > 0);
  let sessionSupportsImages = $derived.by(() => {
    if (!selectedSession) {
      return false;
    }

    const providerSupportsImages = (provider: string) =>
      provider === 'codex' || provider === 'openai_compatible';

    if (selectedSession.route_id) {
      return (
        selectedRoute?.targets.some((target) => providerSupportsImages(target.provider)) ??
        providerSupportsImages(selectedSession.provider)
      );
    }

    return providerSupportsImages(selectedSession.provider);
  });
  let composerHint = $derived.by(() => {
    if (!selectedSession) {
      return 'Select a session from the sidebar to continue.';
    }

    if (!sessionSupportsImages) {
      return 'This session cannot accept image attachments until it uses an image-capable profile or provider.';
    }

    if (
      selectedSession.route_id &&
      selectedSession.provider !== 'codex' &&
      selectedSession.provider !== 'openai_compatible'
    ) {
      return 'Image prompts on this route will switch onto an image-capable target automatically.';
    }

    return 'Drop, paste, or attach images directly into the next turn.';
  });
  let statusLabel = $derived.by(() => {
    if (loading) return 'Connecting';
    if (streamStatus === 'reconnecting') return 'Reconnecting';
    if (streamStatus === 'connecting') return 'Connecting';
    if (error) return 'Degraded';
    return 'Live';
  });
  let activeBrowserPage = $derived.by(() => {
    if (!browserContext || browserContext.session_id !== selectedSessionId) {
      return null;
    }

    return browserContext.pages.find((page) => page.id === browserContext?.active_page_id) ?? null;
  });
  let activePromptProgress = $derived(promptProgress[promptProgress.length - 1] ?? null);
  let composerActivityJobSummary = $derived.by(
    () => jobSummaries.find((job) => jobIsActive(job.state)) ?? jobSummaries[0] ?? null
  );
  let composerActivityUserError = $derived(
    composerActivityJobSummary?.user_error ?? activityJobDetail?.job.user_error ?? selectedSessionUserError
  );
  let composerActivityJobId = $derived(composerActivityJobSummary?.id ?? '');
  let composerActivityPendingApproval = $derived.by(() =>
    latestPendingApproval(activityJobDetail?.approvals ?? [])
  );
  let composerActivityToolCall = $derived.by(() =>
    latestToolCallByStatus(activityJobDetail?.tool_calls ?? [], [
      'running',
      'queued',
      'pending_approval'
    ])
  );
  let composerActivityCommandSession = $derived.by(() =>
    latestByState(activityJobDetail?.command_sessions ?? [], ['running', 'starting'])
  );
  let composerActivityWorker = $derived.by(() =>
    latestByState(activityJobDetail?.workers ?? [], ['running', 'queued', 'paused'])
  );
  let composerActivitySummary = $derived.by(() => {
    if (activePromptProgress) {
      return {
        title: activePromptProgress.label || 'Working on your prompt',
        detail: activePromptProgress.detail || 'Nucleus is preparing the next turn.',
        state: activePromptProgress.status
      };
    }

    if (composerActivityPendingApproval) {
      const toolCall = toolCallForApproval(
        composerActivityPendingApproval,
        activityJobDetail?.tool_calls ?? []
      );
      return {
        title: `Approval required: ${toolCall ? formatActionLabel(toolCall.tool_id) : formatApprovalSummary(composerActivityPendingApproval)}`,
        detail: toolCall
          ? formatToolCallApprovalDetail(toolCall)
          : formatApprovalDetail(composerActivityPendingApproval),
        state: composerActivityPendingApproval.state
      };
    }

    if (composerActivityCommandSession) {
      return {
        title: formatCommandSessionSummary(composerActivityCommandSession),
        detail: formatCommandInvocation(composerActivityCommandSession),
        state: composerActivityCommandSession.state
      };
    }

    if (composerActivityToolCall) {
      return {
        title: formatActionLabel(composerActivityToolCall.tool_id),
        detail: formatToolCallSummary(composerActivityToolCall),
        state: composerActivityToolCall.status
      };
    }

    if (composerActivityWorker) {
      return {
        title: composerActivityWorker.title,
        detail: formatWorkerSummary(composerActivityWorker),
        state: composerActivityWorker.state
      };
    }

    if (activityJobDetail) {
      return {
        title: activityJobDetail.job.title,
        detail:
          activityJobDetail.job.result_summary ||
          activityJobDetail.job.prompt_excerpt ||
          activityJobDetail.job.purpose,
        state: activityJobDetail.job.state
      };
    }

    if (composerActivityJobSummary) {
      return {
        title: composerActivityJobSummary.title,
        detail:
          composerActivityJobSummary.result_summary ||
          composerActivityJobSummary.prompt_excerpt ||
          composerActivityJobSummary.purpose,
        state: composerActivityJobSummary.state
      };
    }

    if (selectedSession?.state === 'running' || selectedSession?.state === 'paused') {
      return {
        title: 'Working on your prompt',
        detail: 'Nucleus is preparing the next turn.',
        state: selectedSession.state
      };
    }

    return null;
  });
  let composerActivityDisplay = $derived.by(() => {
    if (composerActivitySummary) {
      return composerActivitySummary;
    }

    if (!selectedSession) {
      return null;
    }

    return {
      title: 'Utility Worker activity',
      detail: 'No active background work for this session.',
      state: 'idle'
    };
  });

  function uniqueId() {
    try {
      return crypto.randomUUID();
    } catch {
      return `img-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
    }
  }

  async function readFileAsDataUrl(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result ?? ''));
      reader.onerror = () => reject(reader.error ?? new Error('Failed to read image.'));
      reader.readAsDataURL(file);
    });
  }

  function badgeVariantForSession(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'active') return 'default';
    if (state === 'running') return 'warning';
    if (state === 'paused') return 'warning';
    if (state === 'archived') return 'secondary';
    return 'destructive';
  }

  function badgeVariantForJobState(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'completed' || state === 'approved') return 'default';
    if (state === 'paused' || state === 'running' || state === 'queued' || state === 'pending') {
      return 'warning';
    }
    if (state === 'canceled') return 'secondary';
    return 'destructive';
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

  function badgeVariantForVerification(
    status: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (status === 'passed') return 'default';
    if (status === 'failed') return 'destructive';
    if (status === 'pending') return 'warning';
    return 'secondary';
  }

  function formatVerificationStatus(status: string): string {
    if (status === 'passed') return 'Browser-verified';
    if (status === 'failed') return 'Browser verification failed';
    if (status === 'unavailable') return 'Verification unavailable';
    if (status === 'not_performed') return 'Not browser-verified';
    if (status === 'pending') return 'Verification pending';
    return 'Not required';
  }

  function badgeVariantForToolCall(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (state === 'completed') return 'default';
    if (state === 'running' || state === 'queued' || state === 'pending_approval') return 'warning';
    if (state === 'canceled' || state === 'denied' || state === 'closed' || state === 'orphaned') {
      return 'secondary';
    }
    return 'destructive';
  }

  function badgeVariantForAuditStatus(
    status: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (status === 'success') return 'default';
    if (status === 'info') return 'secondary';
    if (status === 'warning') return 'warning';
    return 'destructive';
  }

  function badgeVariantForActionRisk(
    risk: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (risk === 'safe') return 'default';
    if (risk === 'caution') return 'warning';
    return 'secondary';
  }

  function jobIsActive(state: string) {
    return state === 'running' || state === 'queued' || state === 'paused';
  }

  function lastItem<T>(items: T[]) {
    return items.length > 0 ? items[items.length - 1] : null;
  }

  function latestByState<T extends { state: string }>(items: T[], states: string[]) {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (states.includes(items[index].state)) {
        return items[index];
      }
    }

    return lastItem(items);
  }

  function latestToolCallByStatus(items: ToolCallSummary[], statuses: string[]) {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (statuses.includes(items[index].status)) {
        return items[index];
      }
    }

    return lastItem(items);
  }

  function turnRoleLabel(turn: SessionTurn) {
    if (turn.role === 'assistant') return 'Nucleus';
    if (turn.role === 'user') return 'You';
    return formatState(turn.role);
  }

  function turnBubbleClass(turn: SessionTurn) {
    if (turn.role === 'user') {
      return 'border-lime-300/20 bg-lime-300/10 text-zinc-50';
    }

    if (turn.role === 'assistant') {
      return 'border-zinc-800 bg-zinc-900/85 text-zinc-100';
    }

    return 'border-zinc-800 bg-zinc-950/80 text-zinc-300';
  }

  function turnStackClass(turn: SessionTurn) {
    return turn.role === 'user' ? 'items-end' : 'items-start';
  }

  function turnRowClass(turn: SessionTurn) {
    return turn.role === 'user' ? 'justify-end' : 'justify-start';
  }

  function formatPromptProgressStatus(status: string) {
    if (status === 'queued') return 'Queued';
    if (status === 'assembling') return 'Assembling';
    if (status === 'routing') return 'Routing';
    if (status === 'calling') return 'Calling';
    if (status === 'thinking') return 'Thinking';
    if (status === 'streaming') return 'Streaming';
    if (status === 'retrying') return 'Retrying';
    if (status === 'completed') return 'Completed';
    if (status === 'failed') return 'Failed';
    return formatState(status);
  }

  function badgeVariantForPromptStatus(
    status: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (status === 'completed') return 'default';
    if (status === 'failed') return 'destructive';
    if (status === 'retrying') return 'warning';
    return 'secondary';
  }

  function browserLocationLabel(value?: string | null) {
    const raw = value?.trim();
    if (!raw) return 'No page loaded';
    if (raw === 'about:blank') return 'New tab';
    try {
      const parsed = new URL(raw);
      return parsed.hostname || parsed.pathname || raw;
    } catch {
      return raw;
    }
  }

  function browserTabLabel(page: { title: string; url: string }) {
    if (page.title.trim()) return page.title;
    return browserLocationLabel(page.url);
  }

  function browserAddressValue(value?: string | null) {
    const raw = value?.trim() ?? '';
    return raw === 'about:blank' ? '' : raw;
  }

  function browserViewportLabel(mode: BrowserViewportMode) {
    if (mode === 'mobile') return 'Mobile';
    if (mode === 'desktop') return 'Desktop';
    if (mode === 'wide') return 'Wide';
    return 'Fit';
  }

  function browserViewportDescription(mode: BrowserViewportMode) {
    if (mode === 'mobile') return 'Mobile CSS width, scaled into the drawer.';
    if (mode === 'desktop') return 'Desktop CSS width, scaled into the drawer.';
    if (mode === 'wide') return 'Wide desktop CSS width, scaled into the drawer.';
    return 'Match the drawer size exactly.';
  }

  function badgeVariantForActivityState(
    state: string
  ): 'default' | 'secondary' | 'warning' | 'destructive' {
    if (
      state === 'queued' ||
      state === 'running' ||
      state === 'paused' ||
      state === 'assembling' ||
      state === 'routing' ||
      state === 'calling' ||
      state === 'thinking' ||
      state === 'streaming' ||
      state === 'retrying' ||
      state === 'starting' ||
      state === 'pending' ||
      state === 'pending_approval'
    ) {
      return 'warning';
    }

    if (state === 'completed' || state === 'approved') {
      return 'default';
    }

    if (state === 'canceled' || state === 'closed' || state === 'orphaned' || state === 'denied') {
      return 'secondary';
    }

    if (state === 'failed' || state === 'error') {
      return 'destructive';
    }

    return 'secondary';
  }

  function syncActionForms(nextActions: ActionSummary[]) {
    const nextValues: Record<string, Record<string, string>> = {};

    for (const action of nextActions) {
      const existing = actionFormValues[action.id] ?? {};
      const params: Record<string, string> = {};

      for (const parameter of action.parameters) {
        params[parameter.name] = existing[parameter.name] ?? parameter.default_value;
      }

      nextValues[action.id] = params;
    }

    actionFormValues = nextValues;
  }

  function setActionFormValue(actionId: string, name: string, value: string) {
    const existing = actionFormValues[actionId] ?? {};
    actionFormValues = {
      ...actionFormValues,
      [actionId]: {
        ...existing,
        [name]: value
      }
    };
  }

  function setSessionDrafts(session: SessionSummary | null) {
    draftTitle = session?.title ?? '';
    draftProfileId = session?.profile_id ?? '';
    draftApprovalMode = normalizeApprovalMode(session?.approval_mode);
    draftExecutionMode = normalizeExecutionMode(session?.execution_mode);
    draftRunBudgetMode = normalizeRunBudgetMode(session?.run_budget_mode);
    composerModeMenuOpen = false;
    runBudgetMenuOpen = false;
  }

  function upsertJobSummary(next: JobSummary) {
    const remaining = jobSummaries.filter((job) => job.id !== next.id);
    jobSummaries = [next, ...remaining].sort((left, right) => {
      if (right.updated_at !== left.updated_at) {
        return right.updated_at - left.updated_at;
      }

      return right.created_at - left.created_at;
    });
  }

  function syncJobDetail(next: JobDetail | null) {
    jobDetail = next;
    if (next) {
      selectedJobId = next.job.id;
      upsertJobSummary(next.job);
      if (next.job.id === composerActivityJobId) {
        activityJobDetail = next;
      }
    } else if (!composerActivityJobId) {
      activityJobDetail = null;
    }
  }

  function buildNextProjectState(
    projectId: string,
    intent: 'attach' | 'detach' | 'primary'
  ): { projectIds: string[]; primaryProjectId: string | undefined; message: string } {
    const currentIds = attachedProjects.map((project) => project.id);
    const currentPrimaryId = selectedProject?.id;

    if (intent === 'attach') {
      const projectIds = currentIds.includes(projectId) ? currentIds : [...currentIds, projectId];
      return {
        projectIds,
        primaryProjectId: currentPrimaryId ?? projectId,
        message:
          currentIds.length === 0
            ? 'Session moved onto the selected project.'
            : 'Project attached to the session.'
      };
    }

    if (intent === 'primary') {
      return {
        projectIds: currentIds,
        primaryProjectId: projectId,
        message: 'Working directory updated for the session.'
      };
    }

    const projectIds = currentIds.filter((id) => id !== projectId);
    const primaryProjectId =
      currentPrimaryId === projectId ? (projectIds[0] ?? undefined) : currentPrimaryId ?? undefined;

    return {
      projectIds,
      primaryProjectId,
      message:
        projectIds.length === 0
          ? 'Session returned to workspace scratch.'
          : 'Project detached from the session.'
    };
  }

  function upsertSession(next: SessionSummary) {
    if (!overview) {
      return;
    }

    const remaining = overview.sessions.filter((session) => session.id !== next.id);
    overview = {
      ...overview,
      sessions: [next, ...remaining].sort((left, right) => {
        if (right.updated_at !== left.updated_at) {
          return right.updated_at - left.updated_at;
        }

        return right.created_at - left.created_at;
      })
    };
  }

  function syncSession(next: SessionDetail) {
    const previousId = selectedSessionId;
    detail = next;
    selectedSessionId = next.session.id;
    if (previousId && previousId !== next.session.id) {
      resetBrowserState();
    }
    setSessionDrafts(next.session);
    upsertSession(next.session);

    if (next.session.state !== 'running') {
      promptProgress = promptProgress.filter((step) => step.session_id !== next.session.id);
    }
  }

  function syncOverview(nextOverview: RuntimeOverview) {
    overview = nextOverview;

    const preferredId = requestedSessionId || selectedSessionId || nextOverview.sessions[0]?.id || '';

    if (!preferredId) {
      selectedSessionId = '';
      detail = null;
      jobSummaries = [];
      jobDetail = null;
      selectedJobId = '';
      setSessionDrafts(null);
      return;
    }

    const exists = nextOverview.sessions.some((session) => session.id === preferredId);

    if (!exists) {
      selectedSessionId = nextOverview.sessions[0]?.id ?? '';
      if (!selectedSessionId) {
        detail = null;
        setSessionDrafts(null);
      }
      return;
    }

    selectedSessionId = preferredId;

    if (detail && detail.session.id === preferredId) {
      detail = {
        ...detail,
        session: nextOverview.sessions.find((session) => session.id === preferredId) ?? detail.session
      };
    }
  }

  function clearComposerState() {
    promptText = '';
    promptImages = [];
    dragOver = false;

    if (fileInputElement) {
      fileInputElement.value = '';
    }
  }

  function resetBrowserState() {
    if (browserPointerMoveFrame && browser) {
      cancelAnimationFrame(browserPointerMoveFrame);
      browserPointerMoveFrame = 0;
    }
    browserContext = null;
    browserSnapshot = null;
    browserError = null;
    browserLoading = false;
    browserUrl = '';
    browserUrlEditing = false;
    browserStarting = false;
    browserTabChanging = false;
    browserReadyRequestKey = '';
    browserStreamPageId = '';
    browserAnnotationDraft = null;
    browserAnnotationComment = '';
    browserAnnotationSaving = false;
    browserPendingPointerMove = null;
    browserInputQueue = Promise.resolve();
  }

  function openDetailDrawer() {
    sideDrawerMode = 'details';
    sideDrawerOpen = true;
  }

  function openJobDetails(jobId?: string) {
    const targetJobId = jobId ?? jobDetail?.job.id ?? selectedJobSummary?.id ?? '';
    openDetailDrawer();
    if (targetJobId) {
      void loadJob(targetJobId, true);
    }
  }

  function openBrowserDrawer() {
    sideDrawerMode = 'browser';
    sideDrawerOpen = true;
    void ensureBrowserReady();
  }

  function stagePromptProgress(update: PromptProgressUpdate) {
    const next = [...promptProgress, update];
    promptProgress = next.slice(-8);
  }

  function beginOptimisticPrompt(session: SessionSummary, prompt: string, images: ComposerImage[]) {
    const now = Math.floor(Date.now() / 1000);
    const optimisticUserTurn: SessionTurn = {
      id: `optimistic-user:${uniqueId()}`,
      session_id: session.id,
      role: 'user',
      content: prompt.trim(),
      images: images.map(({ display_name, mime_type, data_url }) => ({
        display_name,
        mime_type,
        data_url
      })),
      created_at: now
    };

    if (detail?.session.id === session.id) {
      detail = {
        session: {
          ...detail.session,
          state: 'running',
          last_error: '',
          user_error: null,
          turn_count: detail.session.turn_count + 1,
          updated_at: now,
          last_message_excerpt: prompt.trim()
        },
        turns: [...detail.turns, optimisticUserTurn]
      };
    }

    upsertSession({
      ...session,
      state: 'running',
      last_error: '',
      user_error: null,
      turn_count: session.turn_count + 1,
      updated_at: now,
      last_message_excerpt: prompt.trim()
    });

    promptProgress = [
      {
        session_id: session.id,
        status: 'queued',
        label: 'Sending to Nucleus',
        detail: images.length
          ? `Passing prompt with ${images.length} image attachment(s).`
          : 'Passing prompt from the composer.',
        provider: session.provider,
        model: session.model,
        profile_id: session.profile_id,
        profile_title: session.profile_title,
        route_id: session.route_id,
        route_title: session.route_title,
        attempt: 0,
        attempt_count: 0,
        created_at: now
      }
    ];
  }

  function extractImageFiles(source: DataTransfer | null) {
    if (!source) {
      return [] as File[];
    }

    if (source.items?.length) {
      return Array.from(source.items)
        .filter((item) => item.kind === 'file' && item.type.startsWith('image/'))
        .map((item) => item.getAsFile())
        .filter((file): file is File => file instanceof File);
    }

    return Array.from(source.files).filter((file) => file.type.startsWith('image/'));
  }

  async function addImageFiles(files: File[]) {
    if (!selectedSession) {
      error = 'Select a session before attaching images.';
      return;
    }

    if (!sessionSupportsImages) {
      error = 'This session needs an image-capable profile or provider before it can accept image attachments.';
      return;
    }

    if (!files.length) {
      return;
    }

    const validFiles = files.filter((file) => file.size <= MAX_IMAGE_SIZE_BYTES);

    if (validFiles.length < files.length) {
      error = 'Images must be under 10 MB each.';
    }

    const remaining = MAX_IMAGES - promptImages.length;

    if (remaining <= 0) {
      error = `You can attach up to ${MAX_IMAGES} images per turn.`;
      return;
    }

    const toAdd = validFiles.slice(0, remaining);

    if (!toAdd.length) {
      return;
    }

    const currentSize = promptImages.reduce((sum, image) => sum + image.size_bytes, 0);
    const prepared: ComposerImage[] = [];
    let runningSize = currentSize;

    for (const file of toAdd) {
      if (runningSize + file.size > MAX_TOTAL_IMAGE_SIZE_BYTES) {
        error = 'Attached images cannot exceed 50 MB total per turn.';
        break;
      }

      const dataUrl = await readFileAsDataUrl(file);
      prepared.push({
        id: uniqueId(),
        display_name: file.name,
        mime_type: file.type,
        data_url: dataUrl,
        size_bytes: file.size
      });
      runningSize += file.size;
    }

    if (prepared.length > 0) {
      promptImages = [...promptImages, ...prepared];
      error = null;
    }
  }

  function removeImage(imageId: string) {
    promptImages = promptImages.filter((image) => image.id !== imageId);
  }

  function triggerImagePicker() {
    fileInputElement?.click();
  }

  function resizeComposerTextarea() {
    if (!composerTextareaElement) {
      return;
    }

    composerTextareaElement.style.height = 'auto';
    const nextHeight = Math.min(composerTextareaElement.scrollHeight, 168);
    composerTextareaElement.style.height = `${nextHeight}px`;
    composerTextareaElement.style.overflowY =
      composerTextareaElement.scrollHeight > nextHeight ? 'auto' : 'hidden';
  }

  async function handleFileInputChange(event: Event) {
    const target = event.currentTarget as HTMLInputElement | null;
    const files = Array.from(target?.files ?? []).filter((file) => file.type.startsWith('image/'));
    await addImageFiles(files);

    if (target) {
      target.value = '';
    }
  }

  async function handleComposerPaste(event: ClipboardEvent) {
    const files = extractImageFiles(event.clipboardData);

    if (files.length > 0) {
      event.preventDefault();
      await addImageFiles(files);
    }
  }

  function handleComposerDragOver(event: DragEvent) {
    event.preventDefault();
    dragOver = true;
  }

  function handleComposerDragLeave(event: DragEvent) {
    if (!event.currentTarget) {
      dragOver = false;
      return;
    }

    const nextTarget = event.relatedTarget as Node | null;

    if (!nextTarget || !(event.currentTarget as HTMLElement).contains(nextTarget)) {
      dragOver = false;
    }
  }

  async function handleComposerDrop(event: DragEvent) {
    event.preventDefault();
    dragOver = false;
    await addImageFiles(extractImageFiles(event.dataTransfer));
  }

  async function scrollTranscriptToBottom() {
    await tick();

    if (!transcriptElement) {
      return;
    }

    transcriptElement.scrollTo({
      top: transcriptElement.scrollHeight,
      behavior: 'smooth'
    });
  }

  $effect(() => {
    if (!overview) {
      return;
    }

    const targetId = requestedSessionId || overview.sessions[0]?.id || '';

    if (!targetId) {
      if (selectedSessionId || detail) {
        selectedSessionId = '';
        detail = null;
        resetBrowserState();
        jobSummaries = [];
        jobDetail = null;
        selectedJobId = '';
        setSessionDrafts(null);
      }
      return;
    }

    if (sessionLoading || sessionRequestInFlight === targetId) {
      return;
    }

    if (selectedSessionId === targetId && detail?.session.id === targetId) {
      return;
    }

    void loadSelectedSession(targetId, true);
  });

  $effect(() => {
    const sessionId = detail?.session.id ?? '';
    const turnCount = detail?.turns.length ?? 0;

    if (!sessionId) {
      transcriptAnchor = '';
      return;
    }

    const nextAnchor = `${sessionId}:${turnCount}`;

    if (transcriptAnchor === nextAnchor) {
      return;
    }

    transcriptAnchor = nextAnchor;
    void scrollTranscriptToBottom();
  });

  $effect(() => {
    const jobId = composerActivityJobId;

    if (!jobId) {
      activityJobDetail = null;
      activityJobRequestInFlight = '';
      return;
    }

    if (jobDetail?.job.id === jobId) {
      activityJobDetail = jobDetail;
      return;
    }

    if (activityJobDetail?.job.id === jobId || activityJobRequestInFlight === jobId) {
      return;
    }

    void loadActivityJob(jobId);
  });

  $effect(() => {
    promptText;
    void tick().then(resizeComposerTextarea);
  });

  async function loadSelectedSession(sessionId: string, silent = false) {
    if (!sessionId) {
      selectedSessionId = '';
      detail = null;
      jobSummaries = [];
      jobDetail = null;
      activityJobDetail = null;
      activityJobRequestInFlight = '';
      composerActivityExpanded = false;
      selectedJobId = '';
      setSessionDrafts(null);
      resetBrowserState();
      return;
    }

    const previousId = selectedSessionId;
    selectedSessionId = sessionId;
    sessionRequestInFlight = sessionId;

    if (previousId !== sessionId) {
      clearComposerState();
      resetBrowserState();
      promptProgress = [];
      activityJobDetail = null;
      activityJobRequestInFlight = '';
      composerActivityExpanded = false;
    }

    if (!silent) {
      sessionLoading = true;
    }

    try {
      detail = await fetchSessionDetail(sessionId);
      setSessionDrafts(detail.session);
      await loadSessionJobs(sessionId, true);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the selected session.';
    } finally {
      sessionLoading = false;
      sessionRequestInFlight = '';
    }
  }

  async function loadAll() {
    try {
      const [nextOverview, nextActions, nextAudit] = await Promise.all([
        fetchOverview(),
        fetchActions(),
        fetchAuditEvents(DEFAULT_AUDIT_LIMIT)
      ]);

      actions = nextActions;
      auditEvents = nextAudit;
      syncActionForms(nextActions);
      syncOverview(nextOverview);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load session state.';
    } finally {
      loading = false;
    }
  }

  async function loadSessionJobs(sessionId: string, silent = false) {
    if (!sessionId) {
      jobSummaries = [];
      jobDetail = null;
      selectedJobId = '';
      return;
    }

    if (!silent) {
      jobLoading = true;
    }

    try {
      const nextJobs = await fetchSessionJobs(sessionId);
      jobSummaries = nextJobs;

      if (nextJobs.length === 0) {
        jobDetail = null;
        activityJobDetail = null;
        activityJobRequestInFlight = '';
        selectedJobId = '';
        return;
      }

      const preferredJobId =
        (selectedJobId && nextJobs.some((job) => job.id === selectedJobId) && selectedJobId) ||
        nextJobs[0]?.id ||
        '';

      if (preferredJobId) {
        await loadJob(preferredJobId, true);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the session jobs.';
    } finally {
      jobLoading = false;
    }
  }

  async function loadJob(jobId: string, silent = false) {
    if (!jobId) {
      selectedJobId = '';
      jobDetail = null;
      return;
    }

    selectedJobId = jobId;

    if (!silent) {
      jobLoading = true;
    }

    try {
      syncJobDetail(await fetchJobDetail(jobId));
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load the selected job.';
    } finally {
      jobLoading = false;
    }
  }

  async function loadActivityJob(jobId: string) {
    if (!jobId) {
      activityJobDetail = null;
      activityJobRequestInFlight = '';
      return;
    }

    if (activityJobRequestInFlight === jobId) {
      return;
    }

    activityJobRequestInFlight = jobId;

    try {
      const next = await fetchJobDetail(jobId);
      if (composerActivityJobId === jobId) {
        activityJobDetail = next;
      }
    } catch {
      if (composerActivityJobId === jobId) {
        activityJobDetail = null;
      }
    } finally {
      if (activityJobRequestInFlight === jobId) {
        activityJobRequestInFlight = '';
      }
    }
  }

  async function handleSaveSessionSettings() {
    if (!selectedSession) {
      return;
    }

    if (!draftTitle.trim()) {
      error = 'Session title is required.';
      return;
    }

    savingSession = true;
    deleteConfirmId = null;
    actionConfirmId = null;

    try {
      const next = await updateSession(selectedSession.id, {
        title: draftTitle,
        profile_id: draftProfileId || undefined,
        approval_mode: draftApprovalMode,
        execution_mode: draftExecutionMode,
        run_budget_mode: draftRunBudgetMode
      });

      syncSession(next);
      actionResultMessage = 'Session details saved.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to save the session details.';
    } finally {
      savingSession = false;
    }
  }

  async function handleSelectComposerMode(mode: SessionComposerMode) {
    if (!selectedSession || savingSession) {
      return;
    }

    const currentMode = sessionComposerMode(selectedSession);
    if (mode === currentMode) {
      composerModeMenuOpen = false;
      return;
    }

    const nextApprovalMode = mode === 'trusted' ? 'trusted' : 'ask';
    const nextExecutionMode = mode === 'plan' ? 'plan' : 'act';
    savingSession = true;
    composerModeMenuOpen = false;
    deleteConfirmId = null;
    actionConfirmId = null;

    try {
      const next = await updateSession(selectedSession.id, {
        approval_mode: nextApprovalMode,
        execution_mode: nextExecutionMode
      });

      syncSession(next);
      draftApprovalMode = normalizeApprovalMode(next.session.approval_mode);
      draftExecutionMode = normalizeExecutionMode(next.session.execution_mode);
      actionResultMessage =
        mode === 'plan'
          ? 'Plan mode enabled for this session.'
          : mode === 'trusted'
            ? 'Nucleus can run actions in this session.'
            : 'Nucleus will ask before commands and edits.';
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update session mode.';
    } finally {
      savingSession = false;
    }
  }

  async function handleSelectRunBudgetMode(mode: SessionRunBudgetMode) {
    if (!selectedSession || savingSession) {
      return;
    }

    const currentMode = normalizeRunBudgetMode(selectedSession.run_budget_mode);
    if (mode === currentMode) {
      runBudgetMenuOpen = false;
      return;
    }

    savingSession = true;
    runBudgetMenuOpen = false;
    deleteConfirmId = null;
    actionConfirmId = null;

    try {
      const next = await updateSession(selectedSession.id, {
        run_budget_mode: mode
      });

      syncSession(next);
      draftRunBudgetMode = normalizeRunBudgetMode(next.session.run_budget_mode);
      actionResultMessage = `Run budget set to ${runBudgetModeLabel(mode)}.`;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the run budget.';
    } finally {
      savingSession = false;
    }
  }

  async function handleProjectAction(projectId: string, intent: 'attach' | 'detach' | 'primary') {
    if (!selectedSession) {
      error = 'Select a session first.';
      return;
    }

    updatingProjectId = projectId;
    deleteConfirmId = null;
    actionConfirmId = null;

    try {
      const nextState = buildNextProjectState(projectId, intent);
      const next = await updateSession(selectedSession.id, {
        project_ids: nextState.projectIds,
        primary_project_id: nextState.primaryProjectId
      });
      syncSession(next);
      actionResultMessage = nextState.message;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the session projects.';
    } finally {
      updatingProjectId = null;
    }
  }

  async function loadBrowserContext() {
    if (!selectedSessionId) return;
    browserError = null;
    try {
      browserContext = await fetchBrowserContext(selectedSessionId);
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function ensureBrowserReady() {
    if (!selectedSessionId || browserStarting) return;
    browserStarting = true;
    browserError = null;
    try {
      let nextContext = await fetchBrowserContext(selectedSessionId);
      if (nextContext.pages.length === 0) {
        browserLoading = true;
        nextContext = await navigateBrowser(selectedSessionId, { url: 'about:blank' });
      }
      browserContext = nextContext;
      const activePageId = nextContext.active_page_id ?? nextContext.pages[0]?.id ?? '';
      if (activePageId && (!browserSnapshot || browserSnapshot.page_id !== activePageId)) {
        const snapshot = await captureBrowserSnapshot(selectedSessionId, { page_id: activePageId });
        syncBrowserSnapshot(snapshot, true);
      }
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserStarting = false;
      browserLoading = false;
    }
  }

  async function handleBrowserNavigate() {
    const targetUrl = browserUrl.trim();
    if (!selectedSessionId || !targetUrl) return;
    browserUrlEditing = false;
    browserLoading = true;
    browserError = null;
    try {
      browserContext = await navigateBrowser(selectedSessionId, {
        url: targetUrl,
        page_id: browserContext?.session_id === selectedSessionId ? browserContext.active_page_id ?? undefined : undefined
      });
      const snapshot = await captureBrowserSnapshot(selectedSessionId, {
        page_id: browserContext.session_id === selectedSessionId ? browserContext.active_page_id ?? undefined : undefined
      });
      syncBrowserSnapshot(snapshot, true, true);
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserLoading = false;
    }
  }


  async function handleBrowserOpenTab() {
    if (!selectedSessionId) return;
    browserError = null;
    browserUrlEditing = false;
    browserTabChanging = true;
    browserLoading = true;
    try {
      if (browserStreamPageId) {
        await stopBrowserStream(selectedSessionId, { page_id: browserStreamPageId }).catch(() => null);
        browserStreamPageId = '';
      }
      browserContext = await openBrowserTab(selectedSessionId);
      const activePageId = browserContext.active_page_id ?? browserContext.pages[0]?.id ?? '';
      if (activePageId) {
        const snapshot = await captureBrowserSnapshot(selectedSessionId, { page_id: activePageId });
        syncBrowserSnapshot(snapshot, true, true);
      } else {
        browserSnapshot = null;
        browserUrl = '';
      }
      browserAnnotation = null;
      browserAnnotationDraft = null;
      browserAnnotationComment = '';
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserTabChanging = false;
      browserLoading = false;
    }
  }

  async function handleBrowserSelectPage(pageId: string) {
    if (!selectedSessionId) return;
    if (pageId === browserContext?.active_page_id && browserSnapshot?.page_id === pageId) return;
    browserError = null;
    browserUrlEditing = false;
    browserTabChanging = true;
    browserLoading = true;
    try {
      if (browserStreamPageId && browserStreamPageId !== pageId) {
        await stopBrowserStream(selectedSessionId, { page_id: browserStreamPageId }).catch(() => null);
        browserStreamPageId = '';
      }
      browserContext = await selectBrowserPage(selectedSessionId, { page_id: pageId });
      const snapshot = await captureBrowserSnapshot(selectedSessionId, { page_id: pageId });
      syncBrowserSnapshot(snapshot, true, true);
      browserStreamPageId = '';
      browserAnnotation = null;
      browserAnnotationDraft = null;
      browserAnnotationComment = '';
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserTabChanging = false;
      browserLoading = false;
    }
  }

  async function handleBrowserClosePage(pageId: string, event?: MouseEvent) {
    event?.stopPropagation();
    if (!selectedSessionId) return;
    browserError = null;
    browserUrlEditing = false;
    browserTabChanging = true;
    browserLoading = true;
    try {
      if (browserStreamPageId === pageId) {
        await stopBrowserStream(selectedSessionId, { page_id: browserStreamPageId }).catch(() => null);
        browserStreamPageId = '';
      }
      browserContext = await sendBrowserCommand(selectedSessionId, {
        page_id: pageId,
        command: 'close'
      });
      const activePageId = browserContext.active_page_id ?? browserContext.pages[0]?.id ?? '';
      if (activePageId) {
        const snapshot = await captureBrowserSnapshot(selectedSessionId, { page_id: activePageId });
        syncBrowserSnapshot(snapshot, true, true);
      } else {
        browserSnapshot = null;
        browserUrl = '';
      }
      browserAnnotation = null;
      browserAnnotationDraft = null;
      browserAnnotationComment = '';
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserTabChanging = false;
      browserLoading = false;
    }
  }

  async function handleBrowserCommand(command: string) {
    if (!selectedSessionId || !activeBrowserPage) return;
    browserError = null;
    try {
      browserContext = await sendBrowserCommand(selectedSessionId, {
        page_id: activeBrowserPage.id,
        command
      });
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function handleBrowserSnapshot(showLoading = true) {
    if (!selectedSessionId) return;
    if (browserFrameTicking) return;
    browserFrameTicking = true;
    if (showLoading) browserLoading = true;
    browserError = null;
    try {
      browserSnapshot = await captureBrowserSnapshot(selectedSessionId, {
        page_id: browserContext?.session_id === selectedSessionId ? browserContext.active_page_id ?? undefined : undefined
      });
      browserUrl = browserAddressValue(browserSnapshot.url) || browserUrl;
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserFrameTicking = false;
      if (showLoading) browserLoading = false;
    }
  }


  function syncBrowserContextPage(snapshot: BrowserSnapshot) {
    if (!browserContext || browserContext.session_id !== snapshot.session_id) return;
    let found = false;
    const pages = browserContext.pages.map((page) => {
      if (page.id !== snapshot.page_id) return page;
      found = true;
      return {
        ...page,
        url: snapshot.url || page.url,
        title: snapshot.title || page.title,
        loading: false,
        error: '',
        updated_at: snapshot.captured_at
      };
    });
    if (!found) {
      pages.push({
        id: snapshot.page_id,
        url: snapshot.url,
        title: snapshot.title,
        loading: false,
        error: '',
        updated_at: snapshot.captured_at
      });
    }

    browserContext = {
      ...browserContext,
      active_page_id: snapshot.page_id,
      pages
    };
  }

  function syncBrowserSnapshot(snapshot: BrowserSnapshot, replaceImage = false, forceAddress = false) {
    if (snapshot.session_id !== selectedSessionId) return;
    browserSnapshot = {
      ...snapshot,
      screenshot_data_url:
        replaceImage && snapshot.screenshot_data_url
          ? snapshot.screenshot_data_url
          : snapshot.screenshot_data_url || browserSnapshot?.screenshot_data_url || ''
    };
    syncBrowserContextPage(browserSnapshot);
    if (forceAddress || !browserUrlEditing) {
      browserUrl = browserAddressValue(browserSnapshot.url);
    }
  }

  function enqueueBrowserInput(action: string, value?: string | null, snapshot = false) {
    if (!selectedSessionId || !activeBrowserPage) return;
    const sessionId = selectedSessionId;
    const pageId = activeBrowserPage.id;
    browserError = null;
    browserInputQueue = browserInputQueue
      .catch(() => undefined)
      .then(async () => {
        const result = await sendBrowserAction(sessionId, {
          action,
          page_id: pageId,
          value,
          snapshot
        });
        syncBrowserSnapshot(result, snapshot);
      })
      .catch((caught) => {
        browserError = caught instanceof Error ? caught.message : String(caught);
      });
  }

  function browserCoordinates(event: MouseEvent) {
    if (!browserViewportElement) return null;
    const rect = browserViewportElement.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return null;

    const imageWidth = browserViewportElement.naturalWidth || 1280;
    const imageHeight = browserViewportElement.naturalHeight || 900;
    const relativeX = (event.clientX - rect.left) / rect.width;
    const relativeY = (event.clientY - rect.top) / rect.height;
    if (relativeX < 0 || relativeX > 1 || relativeY < 0 || relativeY > 1) return null;
    return {
      x: Math.round(relativeX * imageWidth),
      y: Math.round(relativeY * imageHeight)
    };
  }

  function sendBrowserPointer(action: string, event: MouseEvent) {
    event.preventDefault();
    const point = browserCoordinates(event);
    if (!point) return;
    if (browserAnnotating && action === 'pointer_down') {
      void handleBrowserAnnotation(point);
      return;
    }
    const value = JSON.stringify({ ...point, button: event.button === 2 ? 'right' : 'left' });
    if (action === 'pointer_move') {
      browserPendingPointerMove = value;
      if (browserPointerMoveFrame === 0) {
        browserPointerMoveFrame = requestAnimationFrame(() => {
          browserPointerMoveFrame = 0;
          const next = browserPendingPointerMove;
          browserPendingPointerMove = null;
          if (next) enqueueBrowserInput('pointer_move', next, false);
        });
      }
      return;
    }
    enqueueBrowserInput(action, value, false);
  }

  async function handleBrowserAnnotation(point: { x: number; y: number }) {
    if (!selectedSessionId || !activeBrowserPage) return;
    browserError = null;
    browserAnnotation = null;
    browserAnnotationDraft = { page_id: activeBrowserPage.id, ...point };
    browserAnnotationComment = '';
  }

  async function saveBrowserAnnotation() {
    if (!selectedSessionId || !browserAnnotationDraft || browserAnnotationSaving) return;
    browserAnnotationSaving = true;
    browserError = null;
    try {
      browserAnnotation = await requestBrowserAnnotation(selectedSessionId, {
        page_id: browserAnnotationDraft.page_id,
        payload: {
          x: browserAnnotationDraft.x,
          y: browserAnnotationDraft.y,
          comment: browserAnnotationComment.trim()
        }
      });
      browserAnnotationDraft = null;
      browserAnnotationComment = '';
      if (selectedSessionId) {
        await loadSelectedSession(selectedSessionId, true);
      }
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserAnnotationSaving = false;
    }
  }

  function handleBrowserKeydown(event: KeyboardEvent) {
    if (!activeBrowserPage) return;
    const pressName = browserKeyPressName(event);
    if (pressName) {
      event.preventDefault();
      enqueueBrowserInput('press', pressName, false);
      return;
    }
    if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault();
      enqueueBrowserInput('type', event.key, false);
    }
  }

  function handleBrowserPaste(event: ClipboardEvent) {
    if (!activeBrowserPage) return;
    const text = event.clipboardData?.getData('text/plain') ?? '';
    if (!text) return;
    event.preventDefault();
    enqueueBrowserInput('type', text, false);
  }

  function browserKeyPressName(event: KeyboardEvent) {
    const keyMap: Record<string, string> = {
      Enter: 'Enter',
      Backspace: 'Backspace',
      Tab: 'Tab',
      Escape: 'Escape',
      Delete: 'Delete',
      ArrowUp: 'ArrowUp',
      ArrowDown: 'ArrowDown',
      ArrowLeft: 'ArrowLeft',
      ArrowRight: 'ArrowRight',
      Home: 'Home',
      End: 'End',
      PageUp: 'PageUp',
      PageDown: 'PageDown'
    };
    for (let index = 1; index <= 12; index += 1) {
      keyMap[`F${index}`] = `F${index}`;
    }

    const key = keyMap[event.key] ?? (event.key.length === 1 ? event.key.toUpperCase() : '');
    if (!key) return '';
    const modifiers = [];
    if (event.ctrlKey) modifiers.push('Control');
    if (event.metaKey) modifiers.push('Meta');
    if (event.altKey) modifiers.push('Alt');
    if (event.shiftKey && (modifiers.length > 0 || event.key.length !== 1)) modifiers.push('Shift');
    if (modifiers.length === 0 && event.key.length === 1) return '';
    return [...modifiers, key].join('+');
  }

  function handleBrowserWheel(event: WheelEvent) {
    if (!activeBrowserPage) return;
    event.preventDefault();
    const point = browserCoordinates(event as unknown as MouseEvent) ?? { x: 640, y: 450 };
    enqueueBrowserInput('scroll', JSON.stringify({ x: point.x, y: point.y, delta_y: Math.round(event.deltaY), delta_x: Math.round(event.deltaX) }), false);
  }




  function browserViewportSize() {
    const rect = browserStageElement?.getBoundingClientRect();
    if (!rect || rect.width < 80 || rect.height < 80) return null;
    const drawerWidth = Math.round(rect.width);
    const drawerHeight = Math.round(rect.height);
    if (browserViewportMode !== 'fit') {
      const width =
        browserViewportMode === 'mobile' ? 390 : browserViewportMode === 'wide' ? 1440 : 1280;
      const minHeight = browserViewportMode === 'mobile' ? 640 : 480;
      const height = Math.max(minHeight, Math.round(width * (drawerHeight / drawerWidth)));
      return { width, height };
    }
    return {
      width: drawerWidth,
      height: drawerHeight
    };
  }

  async function handleBrowserViewportMode(mode: BrowserViewportMode) {
    if (browserViewportMode === mode) return;
    browserViewportMode = mode;
    if (!selectedSessionId || !activeBrowserPage) return;
    try {
      if (browserStreamPageId) {
        await stopBrowserStream(selectedSessionId, { page_id: browserStreamPageId }).catch(() => null);
        browserStreamPageId = '';
      }
      const viewport = browserViewportSize();
      if (viewport) {
        browserContext = await sendBrowserCommand(selectedSessionId, {
          page_id: activeBrowserPage.id,
          command: 'set_viewport',
          args: viewport
        });
      }
      await handleBrowserSnapshot(false);
      await ensureBrowserStream();
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function ensureBrowserStream() {
    if (!selectedSessionId || !activeBrowserPage || browserStreamStarting || browserTabChanging) return;
    if (browserStreamPageId === activeBrowserPage.id) return;
    browserStreamStarting = true;
    try {
      if (browserStreamPageId) {
        await stopBrowserStream(selectedSessionId, { page_id: browserStreamPageId }).catch(() => null);
      }
      const viewport = browserViewportSize();
      if (viewport) {
        browserContext = await sendBrowserCommand(selectedSessionId, {
          page_id: activeBrowserPage.id,
          command: 'set_viewport',
          args: viewport
        });
      }
      await startBrowserStream(selectedSessionId, { page_id: activeBrowserPage.id });
      browserStreamPageId = activeBrowserPage.id;
    } catch (caught) {
      browserError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      browserStreamStarting = false;
    }
  }

  $effect(() => {
    const requestKey = browserPanelOpen && selectedSessionId ? selectedSessionId : '';
    if (!requestKey || browserReadyRequestKey === requestKey) return;
    browserReadyRequestKey = requestKey;
    void Promise.resolve().then(() => ensureBrowserReady());
  });

  $effect(() => {
    if (!browserPanelOpen || !selectedSessionId || !activeBrowserPage) return;
    void ensureBrowserStream();
    return () => {
      const pageId = browserStreamPageId;
      const sessionId = selectedSessionId;
      if (pageId && sessionId) {
        void stopBrowserStream(sessionId, { page_id: pageId }).catch(() => null);
      }
      browserStreamPageId = '';
    };
  });

  async function handlePromptSubmit() {
    if (!promptReady || selectedSession?.state === 'running' || selectedSession?.state === 'paused') {
      return;
    }

    sending = true;
    deleteConfirmId = null;
    actionConfirmId = null;

    let submittedSession = selectedSession;

    if (!submittedSession) {
      try {
        const next = await createSession({});
        syncSession(next);
        submittedSession = next.session;
        await goto(`/?session=${next.session.id}`, { noScroll: true, replaceState: true });
      } catch (cause) {
        error = cause instanceof Error ? cause.message : 'Failed to create the session.';
        sending = false;
        return;
      }
    }

    const submittedPrompt = promptText;
    const submittedImages = [...promptImages];

    beginOptimisticPrompt(submittedSession, submittedPrompt, submittedImages);
    clearComposerState();
    window.setTimeout(() => {
      if (sending && selectedSessionId === submittedSession.id) {
        void loadSelectedSession(submittedSession.id, true);
      }
    }, 180);

    try {
      const next = await sendSessionPrompt(submittedSession.id, {
        prompt: submittedPrompt,
        images: submittedImages.map(({ display_name, mime_type, data_url }) => ({
          display_name,
          mime_type,
          data_url
        }))
      });
      syncSession(next);
      await loadSessionJobs(submittedSession.id, true);
      actionResultMessage = null;
      error = null;
    } catch (cause) {
      void loadSelectedSession(submittedSession.id, true);
      error = cause instanceof Error ? cause.message : 'Failed to send the prompt.';
    } finally {
      sending = false;
    }
  }

  async function handleArchiveToggle() {
    if (!selectedSession) {
      return;
    }

    actioning = true;
    deleteConfirmId = null;
    actionConfirmId = null;

    try {
      const next = await updateSession(selectedSession.id, {
        state: selectedSession.state === 'archived' ? 'active' : 'archived'
      });

      syncSession(next);
      actionResultMessage = null;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to update the session state.';
    } finally {
      actioning = false;
    }
  }

  async function handleDeleteSession() {
    if (!selectedSession) {
      return;
    }

    actionConfirmId = null;

    if (deleteConfirmId !== selectedSession.id) {
      deleteConfirmId = selectedSession.id;
      return;
    }

    actioning = true;

    try {
      const deletedId = selectedSession.id;
      await deleteSession(deletedId);
      deleteConfirmId = null;
      clearComposerState();

      if (overview) {
        overview = {
          ...overview,
          sessions: overview.sessions.filter((session) => session.id !== deletedId)
        };
      }

      const fallbackId = overview?.sessions.find((session) => session.id !== deletedId)?.id ?? '';

      await goto(fallbackId ? `/?session=${fallbackId}` : '/', {
        noScroll: true,
        replaceState: true
      });

      actionResultMessage = null;
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to delete the session.';
    } finally {
      actioning = false;
    }
  }

  function buildActionPayload(action: ActionSummary) {
    const values = actionFormValues[action.id] ?? {};
    const params: Record<string, unknown> = {};

    for (const parameter of action.parameters) {
      const raw = (values[parameter.name] ?? '').trim();

      if (!raw) {
        if (parameter.required) {
          throw new Error(`${parameter.label} is required.`);
        }

        continue;
      }

      if (parameter.value_type === 'number') {
        const parsed = Number(raw);

        if (!Number.isInteger(parsed) || parsed <= 0) {
          throw new Error(`${parameter.label} must be a positive integer.`);
        }

        params[parameter.name] = parsed;
        continue;
      }

      params[parameter.name] = raw;
    }

    return params;
  }

  async function handleRunAction(action: ActionSummary) {
    deleteConfirmId = null;

    if (action.requires_confirmation && actionConfirmId !== action.id) {
      actionConfirmId = action.id;
      return;
    }

    actionConfirmId = null;
    actionRunningId = action.id;

    try {
      const response = await runAction(action.id, {
        params: buildActionPayload(action)
      });

      actionResultMessage = response.message;

      if (action.id === 'runtime.refresh' || action.id === 'workspace.sync') {
        await loadAll();
      } else {
        auditEvents = await fetchAuditEvents(DEFAULT_AUDIT_LIMIT);
      }

      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to run the selected action.';
    } finally {
      actionRunningId = null;
    }
  }

  async function openSession(sessionId: string) {
    await goto(`/?session=${sessionId}`, { noScroll: true });
  }

  async function handleCancelJob(jobId?: string) {
    const targetJobId = jobId ?? jobDetail?.job.id ?? '';
    if (!targetJobId || jobActioning) {
      return;
    }

    jobActioning = true;

    try {
      const next = await cancelJob(targetJobId);
      syncJobDetail(next);
      if (selectedSessionId) {
        await loadSelectedSession(selectedSessionId, true);
        await loadSessionJobs(selectedSessionId, true);
      }
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to cancel the job.';
    } finally {
      jobActioning = false;
    }
  }

  async function handleResumeJob(jobId?: string) {
    const targetJobId = jobId ?? jobDetail?.job.id ?? '';
    if (!targetJobId || jobActioning) {
      return;
    }

    jobActioning = true;

    try {
      const next = await resumeJob(targetJobId);
      syncJobDetail(next);
      if (selectedSessionId) {
        await loadSelectedSession(selectedSessionId, true);
        await loadSessionJobs(selectedSessionId, true);
      }
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to resume the job.';
    } finally {
      jobActioning = false;
    }
  }

  async function handleApproveRequest(approval: ApprovalRequestSummary) {
    if (approvalActioningId || !selectedSessionId) {
      return;
    }

    approvalActioningId = approval.id;

    try {
      syncJobDetail(await approveRequest(approval.id));
      await loadSelectedSession(selectedSessionId, true);
      await loadSessionJobs(selectedSessionId, true);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to approve the pending action.';
    } finally {
      approvalActioningId = null;
    }
  }

  async function handleDenyRequest(approval: ApprovalRequestSummary) {
    if (approvalActioningId || !selectedSessionId) {
      return;
    }

    approvalActioningId = approval.id;

    try {
      syncJobDetail(await denyRequest(approval.id));
      await loadSelectedSession(selectedSessionId, true);
      await loadSessionJobs(selectedSessionId, true);
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to deny the pending action.';
    } finally {
      approvalActioningId = null;
    }
  }

  function formatWorkerSummary(worker: WorkerSummary) {
    return `${formatState(worker.provider)}${worker.model ? ` / ${worker.model}` : ''}`;
  }

  function formatJobEvent(event: JobEvent) {
    return event.summary || formatState(event.event_type);
  }

  function formatToolCallSummary(toolCall: ToolCallSummary) {
    if (toolCall.tool_id === 'command.run') {
      return formatToolCallCommandDetail(toolCall) || toolCall.summary || toolCall.tool_id;
    }

    return toolCall.summary || toolCall.tool_id;
  }

  function formatToolCallTitle(toolCall: ToolCallSummary) {
    if (toolCall.tool_id === 'command.run') {
      const commandDetail = formatToolCallCommandDetail(toolCall);
      return commandDetail ? `Run: ${compactText(commandDetail, 96)}` : 'Run command';
    }

    return formatActionLabel(toolCall.tool_id);
  }

  function formatToolCallTiming(toolCall: ToolCallSummary) {
    const started = toolCall.started_at ?? toolCall.created_at;
    const parts = [`Created ${formatDateTime(toolCall.created_at)}`];

    if (started && started !== toolCall.created_at) {
      parts.push(`Started ${formatDateTime(started)}`);
    }

    if (toolCall.completed_at) {
      parts.push(`Completed ${formatDateTime(toolCall.completed_at)}`);
    }

    return parts.join(' · ');
  }

  function normalizeApprovalMode(value: string | undefined): 'ask' | 'trusted' {
    return value === 'trusted' ? 'trusted' : 'ask';
  }

  function normalizeExecutionMode(value: string | undefined): 'act' | 'plan' {
    return value === 'plan' ? 'plan' : 'act';
  }

  function normalizeRunBudgetMode(value: string | undefined): SessionRunBudgetMode {
    if (
      value === 'standard' ||
      value === 'extended' ||
      value === 'marathon' ||
      value === 'unbounded'
    ) {
      return value;
    }

    return 'inherit';
  }

  function setDraftComposerMode(mode: SessionComposerMode) {
    draftApprovalMode = mode === 'trusted' ? 'trusted' : 'ask';
    draftExecutionMode = mode === 'plan' ? 'plan' : 'act';
  }

  function handleDraftComposerModeChange(event: Event) {
    setDraftComposerMode((event.currentTarget as HTMLSelectElement).value as SessionComposerMode);
  }

  function sessionComposerMode(session: SessionSummary): SessionComposerMode {
    if (normalizeExecutionMode(session.execution_mode) === 'plan') {
      return 'plan';
    }

    return normalizeApprovalMode(session.approval_mode) === 'trusted' ? 'trusted' : 'ask';
  }

  function draftComposerMode(): SessionComposerMode {
    if (draftExecutionMode === 'plan') {
      return 'plan';
    }

    return draftApprovalMode === 'trusted' ? 'trusted' : 'ask';
  }

  function composerModeLabel(mode: SessionComposerMode) {
    if (mode === 'plan') return 'Plan';
    if (mode === 'trusted') return 'Auto-Run';
    return 'Ask First';
  }

  function composerModeDescription(mode: SessionComposerMode) {
    if (mode === 'plan') return 'Draft a plan without taking actions.';
    if (mode === 'trusted') return 'Run trusted actions without approval prompts.';
    return 'Ask before commands, edits, and other actions.';
  }

  function runBudgetModeLabel(mode: SessionRunBudgetMode) {
    if (mode === 'inherit') return 'Default';
    if (mode === 'standard') return 'Focused';
    if (mode === 'extended') return 'Extended';
    if (mode === 'marathon') return 'Marathon';
    return 'Unbounded';
  }

  function runBudgetModeDescription(mode: SessionRunBudgetMode) {
    if (mode === 'inherit') return `Use workspace defaults: ${formatBudgetLimits(workspace?.run_budget)}`;
    if (mode === 'standard') return '80 steps · 160 actions · 2h';
    if (mode === 'extended') return '200 steps · 400 actions · 4h';
    if (mode === 'marathon') return '600 steps · 1200 actions · 8h';
    return 'No step, action, or time cap.';
  }

  function runBudgetModeHelp(mode: SessionRunBudgetMode) {
    if (mode === 'inherit') return 'Matches the workspace default configured in Settings.';
    if (mode === 'standard') return 'For normal multi-step chat, debugging, and small edits.';
    if (mode === 'extended') return 'For longer coding or research tasks.';
    if (mode === 'marathon') return 'For several hours of trusted local work.';
    return 'For trusted sessions where Nucleus should keep going until stopped or blocked.';
  }

  function formatBudgetLimits(
    budget: { max_steps: number; max_tool_calls: number; max_wall_clock_secs: number } | null | undefined
  ) {
    if (!budget) {
      return 'workspace default';
    }

    if (
      budget.max_steps === 0 &&
      budget.max_tool_calls === 0 &&
      budget.max_wall_clock_secs === 0
    ) {
      return 'no run cap';
    }

    const hours = Math.round((budget.max_wall_clock_secs / 3600) * 10) / 10;
    return `${budget.max_steps} steps · ${budget.max_tool_calls} actions · ${hours}h`;
  }

  function formatRunBudget(session: SessionSummary) {
    if (normalizeRunBudgetMode(session.run_budget_mode) === 'unbounded') {
      return 'No run cap';
    }

    return formatBudgetLimits(session.run_budget);
  }

  function composerModeTriggerClass(mode: SessionComposerMode) {
    return cn(
      'inline-flex h-9 items-center justify-center gap-2 rounded-md px-2.5 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 disabled:pointer-events-none disabled:opacity-50 sm:px-3',
      mode === 'ask'
        ? 'text-zinc-300 hover:bg-zinc-900 hover:text-zinc-50 focus-visible:ring-zinc-700'
        : 'bg-zinc-900 text-zinc-100 hover:bg-zinc-800 focus-visible:ring-zinc-700'
    );
  }

  function formatActionLabel(toolId: string) {
    if (toolId === 'command.run') return 'Run command';
    if (toolId === 'command.session.open') return 'Open command session';
    if (toolId === 'command.session.write') return 'Send command input';
    if (toolId === 'command.session.close') return 'Close command session';
    if (toolId === 'fs.read_text') return 'Read file';
    if (toolId === 'fs.write_text') return 'Write file';
    if (toolId === 'fs.patch') return 'Edit file';
    if (toolId === 'fs.list') return 'List files';
    if (toolId === 'rg.search') return 'Search files';
    if (toolId === 'project.inspect') return 'Inspect project';
    if (toolId === 'git.status') return 'Check git status';
    if (toolId === 'git.diff') return 'Review git diff';
    if (toolId === 'git.stage_patch') return 'Stage changes';
    if (toolId === 'tests.run') return 'Run checks';
    return toolId;
  }

  function compactText(value: string, maxChars = 180) {
    const collapsed = value.replace(/\s+/g, ' ').trim();
    if (collapsed.length <= maxChars) {
      return collapsed;
    }

    return `${collapsed.slice(0, Math.max(0, maxChars - 1)).trimEnd()}…`;
  }

  function toolCallForApproval(
    approval: ApprovalRequestSummary,
    toolCalls: ToolCallSummary[]
  ): ToolCallSummary | null {
    return toolCalls.find((toolCall) => toolCall.id === approval.tool_call_id) ?? null;
  }

  function objectValue(value: unknown): Record<string, unknown> {
    return value && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  }

  function stringValue(value: unknown) {
    return typeof value === 'string' ? value : '';
  }

  function formatArgs(args: unknown) {
    if (!Array.isArray(args)) {
      return '';
    }

    return args
      .filter((item): item is string => typeof item === 'string')
      .map((item) => (/\s/.test(item) ? JSON.stringify(item) : item))
      .join(' ');
  }

  function formatToolCallCommandDetail(toolCall: ToolCallSummary) {
    const args = objectValue(toolCall.args_json);
    const command = stringValue(args.command);
    const commandArgs = formatArgs(args.args);
    const cwd = stringValue(args.cwd);
    const commandLine = [command, commandArgs].filter(Boolean).join(' ');

    if (!commandLine) {
      return '';
    }

    return cwd ? `${commandLine}  •  ${compactPath(cwd)}` : commandLine;
  }

  function formatToolCallApprovalDetail(toolCall: ToolCallSummary) {
    const args = objectValue(toolCall.args_json);

    if (toolCall.tool_id === 'command.run') {
      const command = stringValue(args.command);
      const commandArgs = formatArgs(args.args);
      const cwd = stringValue(args.cwd);
      const commandLine = compactText([command, commandArgs].filter(Boolean).join(' '), 140);
      return cwd ? `${commandLine} in ${compactPath(cwd)}` : commandLine;
    }

    if (toolCall.tool_id === 'fs.read_text' || toolCall.tool_id === 'fs.write_text') {
      const path = stringValue(args.path);
      return path ? compactPath(path) : formatToolCallSummary(toolCall);
    }

    if (toolCall.tool_id === 'fs.list') {
      const path = stringValue(args.path);
      return path ? `List ${compactPath(path)}` : formatToolCallSummary(toolCall);
    }

    if (toolCall.tool_id === 'rg.search') {
      const pattern = stringValue(args.pattern);
      const path = stringValue(args.path);
      return compactText([pattern && `Search "${pattern}"`, path && `in ${compactPath(path)}`].filter(Boolean).join(' '));
    }

    return formatToolCallSummary(toolCall);
  }

  function formatApprovalTitle(
    approval: ApprovalRequestSummary,
    toolCalls: ToolCallSummary[]
  ) {
    const toolCall = toolCallForApproval(approval, toolCalls);
    return toolCall ? formatActionLabel(toolCall.tool_id) : formatApprovalSummary(approval);
  }

  function formatApprovalDetail(
    approval: ApprovalRequestSummary,
    toolCalls: ToolCallSummary[] = []
  ) {
    const toolCall = toolCallForApproval(approval, toolCalls);
    if (toolCall) {
      return formatToolCallApprovalDetail(toolCall);
    }

    return compactText(approval.detail || 'Nucleus is waiting for an approval response.');
  }

  function formatJsonPreview(value: unknown) {
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }

  function formatApprovalSummary(approval: ApprovalRequestSummary) {
    return approval.summary || approval.detail || approval.tool_call_id;
  }

  function formatArtifactSummary(artifact: ArtifactSummary) {
    return artifact.title || artifact.kind;
  }

  function formatCommandSessionSummary(commandSession: CommandSessionSummary) {
    return commandSession.title || commandSession.command;
  }

  function formatCommandInvocation(commandSession: CommandSessionSummary) {
    if (commandSession.args.length === 0) {
      return commandSession.command;
    }

    return `${commandSession.command} ${commandSession.args.map((arg) => (/\s/.test(arg) ? JSON.stringify(arg) : arg)).join(' ')}`;
  }

  function formatCommandSessionTiming(commandSession: CommandSessionSummary) {
    const parts = [`Created ${formatDateTime(commandSession.created_at)}`];

    if (commandSession.started_at) {
      parts.push(`Started ${formatDateTime(commandSession.started_at)}`);
    }

    if (commandSession.completed_at) {
      parts.push(`Completed ${formatDateTime(commandSession.completed_at)}`);
    }

    if (commandSession.timeout_secs > 0 && commandSession.state === 'running') {
      parts.push(`Timeout ${commandSession.timeout_secs}s`);
    }

    return parts.join(' · ');
  }

  function latestPendingApproval(approvals: ApprovalRequestSummary[]) {
    for (let index = approvals.length - 1; index >= 0; index -= 1) {
      if (approvals[index].state === 'pending') {
        return approvals[index];
      }
    }

    return lastItem(approvals);
  }

  function applyStreamEvent(event: DaemonEvent) {
    if (event.event === 'browser.frame') {
      if (event.data.session_id === selectedSessionId) {
        const activePageId = browserContext?.active_page_id ?? browserStreamPageId;
        if (activePageId && event.data.page_id !== activePageId) {
          return;
        }
        syncBrowserSnapshot({
          session_id: event.data.session_id,
          page_id: event.data.page_id,
          url: event.data.url || browserSnapshot?.url || '',
          title: event.data.title || browserSnapshot?.title || '',
          content: browserSnapshot?.content || '',
          refs: browserSnapshot?.refs || [],
          downloads: browserSnapshot?.downloads || [],
          screenshot_data_url: `data:${event.data.mime};base64,${event.data.image}`,
          captured_at: event.data.captured_at
        }, true);
      }
      return;
    }

    if (event.event === 'overview.updated') {
      syncOverview(event.data);
      loading = false;
      error = null;
      return;
    }

    if (event.event === 'session.updated') {
      if (event.data.session.id === selectedSessionId || event.data.session.id === requestedSessionId) {
        syncSession(event.data);
      } else {
        upsertSession(event.data.session);
      }
      return;
    }

    if (event.event === 'prompt.progress') {
      if (event.data.session_id === selectedSessionId || event.data.session_id === requestedSessionId) {
        stagePromptProgress(event.data);
      }
      return;
    }

    if (
      event.event === 'job.created' ||
      event.event === 'job.updated' ||
      event.event === 'job.completed' ||
      event.event === 'job.failed'
    ) {
      if (event.data.session_id === selectedSessionId || event.data.session_id === requestedSessionId) {
        upsertJobSummary(event.data);
        void loadSessionJobs(event.data.session_id ?? selectedSessionId, true);
      }
      return;
    }

    if (
      event.event === 'worker.updated' ||
      event.event === 'approval.requested' ||
      event.event === 'approval.resolved' ||
      event.event === 'artifact.added' ||
      event.event === 'command_session.updated'
    ) {
      if (jobDetail?.job.id === event.data.job_id || selectedJobSummary?.id === event.data.job_id) {
        void loadJob(event.data.job_id, true);
      }
      if (composerActivityJobId === event.data.job_id) {
        void loadActivityJob(event.data.job_id);
      }
      return;
    }

    if (event.event === 'audit.updated') {
      auditEvents = event.data.slice(0, DEFAULT_AUDIT_LIMIT);
      return;
    }
  }

  async function handleComposerKeydown(event: KeyboardEvent) {
    if (event.key !== 'Enter' || event.shiftKey) {
      return;
    }

    event.preventDefault();
    await handlePromptSubmit();
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

<div class="flex min-h-0 min-w-0 flex-1 self-stretch overflow-hidden">
  <div class="flex min-h-0 min-w-0 flex-1 overflow-hidden border-y border-zinc-900 bg-zinc-950/70 lg:border-x">
    {#if loading && sessions.length === 0}
      <div class="flex flex-1 items-center justify-center px-8">
        <div class="max-w-md text-center">
          <div class="inline-flex h-12 w-12 items-center justify-center rounded-full border border-zinc-800 bg-zinc-900/80">
            <RotateCcw class="size-5 animate-spin text-zinc-400" />
          </div>
          <div class="mt-4 text-lg font-medium text-zinc-100">Connecting to Nucleus</div>
          <div class="mt-2 text-sm text-zinc-500">
            Nucleus is loading sessions, workspace state, and route readiness.
          </div>
        </div>
      </div>
    {:else if !selectedSession}
      <div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
        <div class="flex min-h-0 flex-1 items-center justify-center px-8">
          <div class="max-w-lg text-center">
            <div class="inline-flex h-14 w-14 items-center justify-center rounded-full border border-zinc-800 bg-zinc-900/80">
              <MessageSquare class="size-6 text-zinc-400" />
            </div>
            <div class="mt-4 text-lg font-medium text-zinc-100">Start a session</div>
            <div class="mt-2 text-sm leading-6 text-zinc-500">
              Send a prompt below or choose an existing session from the sidebar.
            </div>
            <div class="mt-4 inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/80 px-3 py-1.5 text-xs text-zinc-400">
              <span>{statusLabel}</span>
              <span class="text-zinc-700">/</span>
              <span>{sessions.length} sessions</span>
            </div>
          </div>
        </div>

        <div class="shrink-0 border-t border-zinc-900 bg-zinc-950/95 px-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] pt-3 sm:px-6">
          <section
            aria-label="Nucleus activity"
            class="mb-3 overflow-hidden rounded-lg border border-zinc-800 bg-zinc-950/95 shadow-2xl shadow-black/25"
          >
            <div class="flex items-center gap-3 px-3 py-2.5">
              <div class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-zinc-900 text-zinc-300">
                <Workflow class="size-4" />
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex min-w-0 items-center gap-2">
                  <div class="truncate text-sm font-medium text-zinc-100">Utility Worker activity</div>
                  <Badge variant="secondary">Idle</Badge>
                </div>
                <div class="mt-0.5 truncate text-xs text-zinc-500">
                  Work details will appear here after the first prompt starts.
                </div>
              </div>
            </div>
          </section>

          <div
            role="group"
            aria-label="Session composer"
            class="rounded-lg border border-zinc-800 bg-zinc-900/85 p-2"
          >
            <div class="flex items-end gap-2">
              <Textarea
                bind:ref={composerTextareaElement}
                bind:value={promptText}
                rows={1}
                class="max-h-[10.5rem] min-h-10 flex-1 resize-none border-0 bg-transparent px-1 py-2 text-sm leading-5 text-zinc-100 focus:border-transparent focus-visible:ring-0"
                placeholder="Send a message..."
                spellcheck={false}
                aria-describedby="starter-composer-hint"
                disabled={sending}
                onkeydown={handleComposerKeydown}
                onpaste={handleComposerPaste}
              ></Textarea>

              <Button
                variant="default"
                size="icon"
                aria-label={sending ? 'Starting session' : 'Start session'}
                disabled={!promptReady || sending}
                onclick={handlePromptSubmit}
              >
                <Send class={cn('size-4', sending && 'animate-pulse')} />
              </Button>
            </div>

            <div id="starter-composer-hint" class="sr-only">
              Press Enter to send. Press Shift and Enter to add a new line.
            </div>
          </div>
        </div>
      </div>
    {:else}
      <div class="relative flex min-h-0 min-w-0 flex-1 overflow-hidden lg:overflow-hidden">
        <div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          <header class="sticky top-0 z-20 shrink-0 border-b border-zinc-900 bg-zinc-950/90 px-4 py-3 backdrop-blur supports-[backdrop-filter]:bg-zinc-950/75 sm:px-6 sm:py-4">
            <div class="flex items-start gap-3">
              <div class="min-w-0 flex-1">
                <div class="flex min-w-0 flex-wrap items-center gap-2">
                  <div class="truncate text-lg font-semibold text-zinc-50">{selectedSession.title}</div>
                  <Badge variant={badgeVariantForSession(selectedSession.state)}>
                    {formatState(selectedSession.state)}
                  </Badge>
                  {#if selectedSession.provider_session_id}
                    <Badge variant="secondary">Provider thread linked</Badge>
                  {/if}
                </div>
              </div>

              <div class="flex shrink-0 items-start gap-2 self-start">
                {#if sessionLoading}
                  <div class="inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/70 px-3 py-1 text-xs text-zinc-500">
                    <RotateCcw class="size-3.5 animate-spin" />
                    Loading
                  </div>
                {/if}
                <div class="flex flex-col items-center gap-1">
                  <Button
                    variant={detailPanelOpen ? 'secondary' : 'ghost'}
                    size="icon"
                    aria-label="Show session details"
                    aria-pressed={detailPanelOpen}
                    title="Session details"
                    onclick={openDetailDrawer}
                  >
                    <PanelRightOpen class="size-4" />
                  </Button>
                  <Button
                    variant={browserPanelOpen ? 'secondary' : 'ghost'}
                    size="icon"
                    aria-label="Show browser"
                    aria-pressed={browserPanelOpen}
                    title="Browser"
                    onclick={openBrowserDrawer}
                  >
                    <Compass class="size-4" />
                  </Button>
                </div>
              </div>
            </div>

            <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-zinc-500">
              <span class="inline-flex items-center gap-1.5">
                <Workflow class="size-3.5" />
                <span>{selectedSession.profile_title || selectedSession.route_title || 'Direct session'}</span>
              </span>
              <span class="inline-flex items-center gap-1.5">
                <Bot class="size-3.5" />
                <span>{formatState(selectedSession.provider)}</span>
                {#if selectedSession.model}
                  <span class="text-zinc-700">/</span>
                  <span>{selectedSession.model}</span>
                {/if}
              </span>
              <span class="inline-flex items-center gap-1.5">
                <FolderTree class="size-3.5" />
                <span class="min-w-0 max-w-56 truncate">{selectedProjectTitle}</span>
              </span>
              <span>{selectedSession.turn_count} turns</span>
              <span>{formatDateTime(selectedSession.updated_at)}</span>
            </div>
          </header>

          {#if error || actionResultMessage || selectedSessionUserError || selectedSession.last_error}
            <div class="sticky top-[89px] z-10 shrink-0 border-b border-zinc-900 bg-zinc-950/75 px-4 py-3 backdrop-blur supports-[backdrop-filter]:bg-zinc-950/65 sm:top-[105px] sm:px-6">
              {#if error}
                <div class="rounded-lg border border-red-500/25 bg-red-500/10 px-3 py-2 text-sm text-red-200">
                  {error}
                </div>
              {/if}

              {#if !error && actionResultMessage}
                <div class="rounded-lg border border-zinc-800 bg-zinc-900/80 px-3 py-2 text-sm text-zinc-300">
                  {actionResultMessage}
                </div>
              {/if}

              {#if !error && !actionResultMessage && selectedSessionUserError}
                <FriendlyErrorNotice
                  userError={selectedSessionUserError}
                  onRetryJob={() => void handleResumeJob(composerActivityJobSummary?.id)}
                  onCancelJob={() => void handleCancelJob(composerActivityJobSummary?.id)}
                  onOpenJobDetails={() => openJobDetails(composerActivityJobSummary?.id)}
                  retryDisabled={jobActioning}
                  cancelDisabled={jobActioning}
                />
              {:else if !error && !actionResultMessage && selectedSession.last_error}
                <div class="rounded-lg border border-red-500/25 bg-red-500/10 px-3 py-2 text-sm text-red-200">
                  {selectedSession.last_error}
                </div>
              {/if}
            </div>
          {/if}

          <div bind:this={transcriptElement} class="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 py-4 pb-40 sm:px-6 sm:py-6 sm:pb-44">
            {#if detail?.turns.length}
              <div class="space-y-6">
                {#each detail.turns as turn (turn.id)}
                  <div class={cn('flex', turnRowClass(turn))}>
                    <div class={cn('flex max-w-3xl flex-col gap-2', turnStackClass(turn))}>
                      <div class="flex items-center gap-2 text-xs text-zinc-500">
                        <span>{turnRoleLabel(turn)}</span>
                        <span class="text-zinc-700">/</span>
                        <span>{formatDateTime(turn.created_at)}</span>
                      </div>
                      <div class={cn('rounded-2xl border px-4 py-3 shadow-sm', turnBubbleClass(turn))}>
                        {#if turn.images.length > 0}
                          <div class="mb-3 grid gap-3 sm:grid-cols-2">
                            {#each turn.images as image}
                              <div class="overflow-hidden rounded-xl border border-zinc-800 bg-zinc-950/70">
                                <img
                                  src={image.data_url}
                                  alt={image.display_name}
                                  class="aspect-[4/3] w-full object-cover"
                                />
                                <div class="truncate border-t border-zinc-800 px-3 py-2 text-xs text-zinc-400">
                                  {image.display_name}
                                </div>
                              </div>
                            {/each}
                          </div>
                        {/if}

                        {#if turn.content}
                          {#if turn.role === 'assistant'}
                            <MarkdownContent content={turn.content} class="break-words text-zinc-100" />
                          {:else}
                            <div class="break-words whitespace-pre-wrap text-sm leading-6">
                              {turn.content}
                            </div>
                          {/if}
                        {/if}
                      </div>
                    </div>
                  </div>
                {/each}
              </div>
            {:else}
              <div class="flex h-full min-h-[16rem] items-center justify-center sm:min-h-[22rem]">
                <div class="max-w-md text-center">
                  <div class="inline-flex h-12 w-12 items-center justify-center rounded-full border border-zinc-800 bg-zinc-900/80">
                    <Workflow class="size-5 text-zinc-400" />
                  </div>
                  <div class="mt-4 text-lg font-medium text-zinc-100">No turns yet</div>
                  <div class="mt-2 text-sm leading-6 text-zinc-500">
                    Send the first prompt from here. This pane stays dedicated to the conversation once work starts.
                  </div>
                </div>
              </div>
            {/if}
          </div>

          <div class="shrink-0 border-t border-zinc-900 bg-zinc-950/95 px-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] pt-3 sm:px-6">
            {#if composerActivityDisplay}
              <section
                aria-label="Nucleus activity"
                class={cn(
                  'mb-3 overflow-hidden rounded-lg border border-zinc-800 bg-zinc-950/95 shadow-2xl shadow-black/25 transition-[max-height]',
                  composerActivityExpanded ? 'max-h-[min(30rem,46vh)]' : 'max-h-20'
                )}
              >
                <div class="flex flex-col gap-2 px-3 py-2.5 sm:flex-row sm:items-center">
                  <button
                    type="button"
                    class="flex min-w-0 flex-1 items-center gap-3 text-left"
                    aria-expanded={composerActivityExpanded}
                    onclick={() => {
                      composerActivityExpanded = !composerActivityExpanded;
                    }}
                  >
                    <div class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-zinc-900 text-zinc-300">
                      <Workflow class="size-4" />
                    </div>

                    <div class="min-w-0 flex-1">
                      <div class="flex min-w-0 items-center gap-2">
                        <div class="truncate text-sm font-medium text-zinc-100">
                          {composerActivityDisplay.title}
                        </div>
                        <Badge variant={badgeVariantForActivityState(composerActivityDisplay.state)}>
                          {formatPromptProgressStatus(composerActivityDisplay.state)}
                        </Badge>
                      </div>
                      <div class="mt-0.5 truncate text-xs text-zinc-500">
                        {composerActivityDisplay.detail}
                      </div>
                    </div>

                    <div class="hidden shrink-0 flex-wrap justify-end gap-x-3 gap-y-1 text-[11px] text-zinc-600 md:flex">
                      {#if composerActivityJobSummary}
                        <span>{composerActivityJobSummary.worker_count} Utility Worker{composerActivityJobSummary.worker_count === 1 ? '' : 's'}</span>
                        <span>{composerActivityJobSummary.pending_approval_count} approvals</span>
                        <span>{composerActivityJobSummary.artifact_count} artifacts</span>
                      {/if}
                      {#if composerActivityToolCall}
                        <span>Action · {formatActionLabel(composerActivityToolCall.tool_id)}</span>
                      {:else if composerActivityCommandSession}
                        <span>Command · {formatCommandSessionSummary(composerActivityCommandSession)}</span>
                      {:else if composerActivityWorker}
                        <span>Utility Worker · {composerActivityWorker.title}</span>
                      {/if}
                    </div>

                    <div class="shrink-0 text-zinc-500">
                      {#if composerActivityExpanded}
                        <ChevronDown class="size-4" />
                      {:else}
                        <ChevronUp class="size-4" />
                      {/if}
                    </div>
                  </button>

                  {#if composerActivityPendingApproval?.state === 'pending'}
                    <div class="flex shrink-0 gap-2 pl-11 sm:pl-0">
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={approvalActioningId !== null}
                        onclick={() => {
                          void handleApproveRequest(composerActivityPendingApproval);
                        }}
                      >
                        <span>{approvalActioningId === composerActivityPendingApproval.id ? 'Approving' : 'Approve'}</span>
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={approvalActioningId !== null}
                        onclick={() => {
                          void handleDenyRequest(composerActivityPendingApproval);
                        }}
                      >
                        <span>{approvalActioningId === composerActivityPendingApproval.id ? 'Resolving' : 'Deny'}</span>
                      </Button>
                    </div>
                  {/if}
                </div>

                {#if composerActivityExpanded}
                  <div class="border-t border-zinc-800 px-3 pb-3 pt-3">
                    <div class="max-h-[min(24rem,38vh)] space-y-4 overflow-y-auto pr-1">
                      {#if promptProgress.length === 0 && !activityJobDetail}
                        <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3 text-sm text-zinc-500">
                          Utility Worker activity will appear here when Nucleus starts work for this session.
                        </div>
                      {/if}

                      {#if promptProgress.length > 0}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Prompt Progress</div>
                          <div class="mt-2 space-y-2">
                            {#each promptProgress as step, index (index)}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="text-sm text-zinc-100">{step.label}</div>
                                    {#if step.detail}
                                      <div class="mt-1 text-xs leading-5 text-zinc-500">{step.detail}</div>
                                    {/if}
                                  </div>
                                  <Badge variant={badgeVariantForPromptStatus(step.status)}>
                                    {formatPromptProgressStatus(step.status)}
                                  </Badge>
                                </div>
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}

                      {#if activityJobDetail?.job.user_error}
                        <FriendlyErrorNotice
                          userError={activityJobDetail.job.user_error}
                          onRetryJob={() => void handleResumeJob(activityJobDetail?.job.id)}
                          onCancelJob={() => void handleCancelJob(activityJobDetail?.job.id)}
                          onOpenJobDetails={() => openJobDetails(activityJobDetail?.job.id)}
                          retryDisabled={jobActioning}
                          cancelDisabled={jobActioning}
                        />
                      {:else if activityJobDetail?.job.last_error}
                        <div class="rounded-xl border border-red-500/20 bg-red-500/10 px-3 py-2 text-xs leading-5 text-red-200">
                          {activityJobDetail.job.last_error}
                        </div>
                      {/if}

                      {#if activityJobDetail?.workers.length}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Utility Workers</div>
                          <div class="mt-2 space-y-2">
                            {#each [...activityJobDetail.workers].slice(-3).reverse() as worker}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{worker.title}</div>
                                    <div class="mt-1 text-xs text-zinc-500">
                                      {formatWorkerSummary(worker)}
                                    </div>
                                  </div>
                                  <Badge variant={badgeVariantForJobState(worker.state)}>
                                    {formatState(worker.state)}
                                  </Badge>
                                </div>
                                {#if worker.user_error}
                                  <FriendlyErrorNotice userError={worker.user_error} class="mt-2" />
                                {:else if worker.last_error}
                                  <div class="mt-2 text-xs leading-5 text-red-200">{worker.last_error}</div>
                                {/if}
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}

                      {#if activityJobDetail?.tool_calls.length}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Actions</div>
                          <div class="mt-2 space-y-2">
                            {#each [...activityJobDetail.tool_calls].slice(-4).reverse() as toolCall}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{formatToolCallTitle(toolCall)}</div>
                                    <div class="mt-1 text-xs text-zinc-500">{formatToolCallTiming(toolCall)}</div>
                                    {#if formatToolCallSummary(toolCall) !== formatToolCallTitle(toolCall)}
                                      <div class="mt-1 text-xs leading-5 text-zinc-500">
                                        {compactText(formatToolCallSummary(toolCall), 220)}
                                      </div>
                                    {/if}
                                  </div>
                                  <Badge variant={badgeVariantForToolCall(toolCall.status)}>
                                    {formatState(toolCall.status)}
                                  </Badge>
                                </div>
                                {#if toolCall.error_detail}
                                  <div class="mt-2 text-xs leading-5 text-red-200">{toolCall.error_detail}</div>
                                {/if}
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}

                      {#if activityJobDetail?.approvals.length}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Approvals</div>
                          <div class="mt-2 space-y-2">
                            {#each [...activityJobDetail.approvals].slice(-3).reverse() as approval}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{formatApprovalTitle(approval, activityJobDetail?.tool_calls ?? [])}</div>
                                    <div class="mt-1 text-xs leading-5 text-zinc-500">{formatApprovalDetail(approval, activityJobDetail?.tool_calls ?? [])}</div>
                                  </div>
                                  <Badge variant={badgeVariantForJobState(approval.state)}>
                                    {formatState(approval.state)}
                                  </Badge>
                                </div>
                                {#if approval.state === 'pending'}
                                  <div class="mt-3 flex flex-wrap gap-2">
                                    <Button
                                      variant="secondary"
                                      size="sm"
                                      disabled={approvalActioningId !== null}
                                      onclick={() => {
                                        void handleApproveRequest(approval);
                                      }}
                                    >
                                      <span>{approvalActioningId === approval.id ? 'Approving' : 'Approve'}</span>
                                    </Button>
                                    <Button
                                      variant="outline"
                                      size="sm"
                                      disabled={approvalActioningId !== null}
                                      onclick={() => {
                                        void handleDenyRequest(approval);
                                      }}
                                    >
                                      <span>{approvalActioningId === approval.id ? 'Resolving' : 'Deny'}</span>
                                    </Button>
                                  </div>
                                {/if}
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}

                      {#if activityJobDetail?.command_sessions.length}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Command Sessions</div>
                          <div class="mt-2 space-y-2">
                            {#each [...activityJobDetail.command_sessions].slice(-3).reverse() as commandSession}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{formatCommandSessionSummary(commandSession)}</div>
                                    <div class="mt-1 text-xs text-zinc-500">{formatCommandSessionTiming(commandSession)}</div>
                                    <details class="mt-1 text-xs leading-5 text-zinc-500">
                                      <summary class="cursor-pointer select-none text-zinc-400">Command</summary>
                                      <pre class="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-950 px-3 py-2 text-[11px] leading-5 text-zinc-400">{formatCommandInvocation(commandSession)}</pre>
                                    </details>
                                  </div>
                                  <Badge variant={badgeVariantForToolCall(commandSession.state)}>
                                    {formatState(commandSession.state)}
                                  </Badge>
                                </div>
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}

                      {#if activityJobDetail?.artifacts.length}
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Artifacts</div>
                          <div class="mt-2 space-y-2">
                            {#each [...activityJobDetail.artifacts].slice(-2).reverse() as artifact}
                              <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-2">
                                <div class="flex items-start justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{formatArtifactSummary(artifact)}</div>
                                    <div class="mt-1 text-xs text-zinc-500">
                                      {artifact.kind} · {formatDateTime(artifact.created_at)}
                                    </div>
                                  </div>
                                  <div class="shrink-0 text-[11px] text-zinc-600">{artifact.size_bytes} bytes</div>
                                </div>
                                {#if artifact.preview_text}
                                  <pre class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-950 px-3 py-2 text-xs leading-5 text-zinc-500">{artifact.preview_text}</pre>
                                {/if}
                              </div>
                            {/each}
                          </div>
                        </div>
                      {/if}
                    </div>

                    {#if composerActivityJobSummary}
                      <div class="mt-3 flex justify-end border-t border-zinc-800 pt-3">
                        <Button
                          variant="ghost"
                          size="sm"
                          onclick={() => {
                            openDetailDrawer();
                            void loadJob(composerActivityJobSummary.id, true);
                          }}
                        >
                          Open Full Job History
                        </Button>
                      </div>
                    {/if}
                  </div>
                {/if}
              </section>
            {/if}

            {#if selectedSession.state === 'paused' || selectedSession.state === 'error'}
              <section
                class={cn(
                  'mb-3 rounded-xl border px-4 py-3 text-sm',
                  selectedSession.state === 'paused'
                    ? 'border-amber-500/30 bg-amber-500/10 text-amber-100'
                    : 'border-red-500/30 bg-red-500/10 text-red-100'
                )}
              >
                {#if selectedSession.state === 'error' && composerActivityUserError}
                  <FriendlyErrorNotice
                    userError={composerActivityUserError}
                    class="border-0 bg-transparent p-0"
                    onRetryJob={() => void handleResumeJob(composerActivityJobSummary?.id)}
                    onCancelJob={() => void handleCancelJob(composerActivityJobSummary?.id)}
                    onOpenJobDetails={() => openJobDetails(composerActivityJobSummary?.id)}
                    retryDisabled={jobActioning}
                    cancelDisabled={jobActioning}
                  />
                {:else}
                  <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                    <div class="min-w-0">
                      <div class="font-medium">
                        {selectedSession.state === 'paused' ? 'This session is paused.' : 'This session has a recoverable job error.'}
                      </div>
                      <div class="mt-1 text-xs leading-5 opacity-75">
                        {selectedSession.state === 'paused'
                          ? 'Resume or cancel the paused Utility Worker job before sending another prompt.'
                          : 'Retry the checkpointed Utility Worker job or cancel it before continuing this session.'}
                      </div>
                    </div>
                    <div class="flex shrink-0 flex-wrap gap-2">
                      {#if composerActivityJobSummary?.state === 'paused' || composerActivityJobSummary?.state === 'failed'}
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={jobActioning}
                          onclick={() => handleResumeJob(composerActivityJobSummary?.id)}
                        >
                          <RotateCcw class={cn('size-4', jobActioning && 'animate-spin')} />
                          <span>{jobActioning ? 'Retrying' : composerActivityJobSummary?.state === 'failed' ? 'Retry Job' : 'Resume Job'}</span>
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={jobActioning}
                          onclick={() => handleCancelJob(composerActivityJobSummary?.id)}
                        >
                          <XCircle class="size-4" />
                          <span>Cancel Job</span>
                        </Button>
                      {/if}
                      {#if composerActivityJobSummary}
                        <Button
                          variant="ghost"
                          size="sm"
                          onclick={() => openJobDetails(composerActivityJobSummary.id)}
                        >
                          Open Job Details
                        </Button>
                      {/if}
                    </div>
                  </div>
                {/if}
              </section>
            {/if}

            <div
              role="group"
              aria-label="Session composer"
              class={cn(
                'rounded-lg border bg-zinc-900/85 p-2 transition-colors',
                dragOver ? 'border-lime-300/50 bg-lime-300/8' : 'border-zinc-800'
              )}
              ondragover={handleComposerDragOver}
              ondragleave={handleComposerDragLeave}
              ondrop={handleComposerDrop}
            >
              {#if promptImages.length > 0}
                <div class="mb-2 flex gap-2 overflow-x-auto pb-1">
                  {#each promptImages as image}
                    <div class="relative flex h-12 min-w-0 max-w-40 shrink-0 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950/75 p-1 pr-8">
                      <img
                        src={image.data_url}
                        alt={image.display_name}
                        class="h-10 w-10 shrink-0 rounded-md object-cover"
                      />
                      <div class="min-w-0 truncate text-[11px] text-zinc-400">{image.display_name}</div>
                      <button
                        type="button"
                        class="absolute right-1.5 top-1.5 inline-flex h-5 w-5 items-center justify-center rounded-full bg-black/75 text-zinc-100"
                        aria-label={`Remove ${image.display_name}`}
                        onclick={() => removeImage(image.id)}
                      >
                        <X class="size-3" />
                      </button>
                    </div>
                  {/each}
                </div>
              {/if}

              <div class="flex items-end gap-2">
                <input
                  bind:this={fileInputElement}
                  type="file"
                  accept="image/*"
                  multiple
                  class="hidden"
                  onchange={handleFileInputChange}
                />
                <Button
                  variant="ghost"
                  size="icon"
                  class="h-9 w-9"
                  aria-label="Attach image"
                  title={composerHint}
                  disabled={
                    !sessionSupportsImages ||
                    sending ||
                    selectedSession.state === 'archived' ||
                    selectedSession.state === 'running' ||
                    selectedSession.state === 'paused'
                  }
                  onclick={triggerImagePicker}
                >
                  <ImagePlus class="size-4" />
                </Button>

                <DropdownMenu.Root bind:open={composerModeMenuOpen}>
                  <DropdownMenu.Trigger
                    class={composerModeTriggerClass(sessionComposerMode(selectedSession))}
                    aria-label={`Session mode: ${composerModeLabel(sessionComposerMode(selectedSession))}`}
                    title={composerModeDescription(sessionComposerMode(selectedSession))}
                    disabled={savingSession || selectedSession.state === 'archived'}
                  >
                    {#if sessionComposerMode(selectedSession) === 'plan'}
                      <MessageSquare class="size-4" />
                    {:else}
                      <Wrench class="size-4" />
                    {/if}
                    <span class="hidden sm:inline">{composerModeLabel(sessionComposerMode(selectedSession))}</span>
                    <ChevronUp class="size-3.5 text-zinc-500" />
                  </DropdownMenu.Trigger>
                  <DropdownMenu.Content side="top" align="start" sideOffset={8} class="w-64 max-w-[calc(100vw-2rem)]">
                    <DropdownMenu.RadioGroup
                      value={sessionComposerMode(selectedSession)}
                      onValueChange={(value) => {
                        if (value === 'plan' || value === 'ask' || value === 'trusted') {
                          void handleSelectComposerMode(value);
                        }
                      }}
                    >
                      {#each COMPOSER_MODES as mode}
                        <DropdownMenu.RadioItem value={mode} class="items-start gap-3 py-2 pl-2 pr-8">
                          <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                            {#if mode === 'plan'}
                              <MessageSquare class="size-4" />
                            {:else}
                              <Wrench class="size-4" />
                            {/if}
                          </div>
                          <div class="min-w-0">
                            <div class="text-sm font-medium text-zinc-100">
                              {composerModeLabel(mode)}
                            </div>
                            <div class="mt-0.5 text-xs leading-5 text-zinc-500">
                              {composerModeDescription(mode)}
                            </div>
                          </div>
                        </DropdownMenu.RadioItem>
                      {/each}
                    </DropdownMenu.RadioGroup>
                  </DropdownMenu.Content>
                </DropdownMenu.Root>

                <DropdownMenu.Root bind:open={runBudgetMenuOpen}>
                  <DropdownMenu.Trigger
                    class="inline-flex h-9 items-center justify-center gap-2 rounded-md px-2.5 text-xs font-medium text-zinc-300 transition-colors hover:bg-zinc-900 hover:text-zinc-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700 disabled:pointer-events-none disabled:opacity-50 sm:px-3"
                    aria-label={`Run budget: ${runBudgetModeLabel(normalizeRunBudgetMode(selectedSession.run_budget_mode))}`}
                    title={formatRunBudget(selectedSession)}
                    disabled={savingSession || selectedSession.state === 'archived'}
                  >
                    <Clock3 class="size-4" />
                    <span class="hidden sm:inline">{runBudgetModeLabel(normalizeRunBudgetMode(selectedSession.run_budget_mode))}</span>
                    <ChevronUp class="size-3.5 text-zinc-500" />
                  </DropdownMenu.Trigger>
                  <DropdownMenu.Content side="top" align="start" sideOffset={8} class="w-72 max-w-[calc(100vw-2rem)]">
                    <DropdownMenu.RadioGroup
                      value={normalizeRunBudgetMode(selectedSession.run_budget_mode)}
                      onValueChange={(value) => {
                        if (
                          value === 'inherit' ||
                          value === 'standard' ||
                          value === 'extended' ||
                          value === 'marathon' ||
                          value === 'unbounded'
                        ) {
                          void handleSelectRunBudgetMode(value);
                        }
                      }}
                    >
                      {#each RUN_BUDGET_MODES as mode}
                        <DropdownMenu.RadioItem value={mode} class="items-start gap-3 py-2 pl-2 pr-8">
                          <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                            <Clock3 class="size-4" />
                          </div>
                          <div class="min-w-0">
                            <div class="text-sm font-medium text-zinc-100">
                              {runBudgetModeLabel(mode)}
                            </div>
                            <div class="mt-0.5 text-xs leading-5 text-zinc-500">
                              {runBudgetModeDescription(mode)}
                            </div>
                            <div class="mt-0.5 text-xs leading-5 text-zinc-600">
                              {runBudgetModeHelp(mode)}
                            </div>
                          </div>
                        </DropdownMenu.RadioItem>
                      {/each}
                    </DropdownMenu.RadioGroup>
                  </DropdownMenu.Content>
                </DropdownMenu.Root>

                <Textarea
                  bind:ref={composerTextareaElement}
                  bind:value={promptText}
                  rows={1}
                  class="max-h-[10.5rem] min-h-10 flex-1 resize-none border-0 bg-transparent px-1 py-2 text-sm leading-5 text-zinc-100 focus:border-transparent focus-visible:ring-0"
                  placeholder="Send a message..."
                  spellcheck={false}
                  aria-describedby="composer-hint"
                  disabled={
                    sending ||
                    selectedSession.state === 'archived' ||
                    selectedSession.state === 'paused'
                  }
                  onkeydown={handleComposerKeydown}
                  onpaste={handleComposerPaste}
                ></Textarea>

                <Button
                  variant="default"
                  size="icon"
                  aria-label={sending ? 'Sending prompt' : 'Send prompt'}
                  disabled={
                    !promptReady ||
                    sending ||
                    selectedSession.state === 'archived' ||
                    selectedSession.state === 'running' ||
                    selectedSession.state === 'paused'
                  }
                  onclick={handlePromptSubmit}
                >
                  <Send class={cn('size-4', sending && 'animate-pulse')} />
                </Button>
              </div>

              <div id="composer-hint" class="sr-only">
                {composerHint} Press Enter to send. Press Shift and Enter to add a new line.
              </div>
            </div>
          </div>
        </div>

        <Sheet bind:open={sideDrawerOpen}>
          <SheetContent
            portalDisabled
            trapFocus={false}
            preventScroll={false}
            interactOutsideBehavior="ignore"
            overlayClass="lg:hidden"
            class={cn(
              'z-20 border-zinc-900 lg:static lg:z-auto lg:shrink-0 lg:shadow-none',
              sideDrawerMode === 'browser'
                ? 'max-w-2xl overflow-hidden lg:w-[34rem] lg:max-w-[34rem]'
                : 'max-w-md overflow-y-auto overflow-x-hidden lg:w-96 lg:max-w-96'
            )}
          >
            <SheetHeader class="items-center border-zinc-900 px-5 py-4">
              <div>
                <SheetTitle class="text-sm">
                  {sideDrawerMode === 'browser' ? 'Browser' : 'Session Details'}
                </SheetTitle>
                <SheetDescription class="mt-1 text-xs">
                  {sideDrawerMode === 'browser'
                    ? 'Session-scoped browser companion for navigation and agent verification.'
                    : 'Secondary controls live here so the chat stays clear.'}
                </SheetDescription>
              </div>
              <Button variant="ghost" size="icon" aria-label="Close drawer" onclick={() => (sideDrawerOpen = false)}>
                <X class="size-4" />
              </Button>
            </SheetHeader>
            {#if sideDrawerMode === 'browser'}
              <div class="flex h-full min-h-0 flex-col gap-3 px-5 py-5">
              <div class="flex h-10 min-w-0 shrink-0 items-end gap-1 overflow-x-auto border-b border-zinc-800 pt-1">
                {#if (browserContext?.pages.length ?? 0) === 0}
                  <div class="flex h-9 max-w-40 shrink-0 items-center rounded-t-md border border-b-0 border-zinc-800 bg-zinc-950 px-3 text-xs text-zinc-500">
                    {browserStarting || browserLoading ? 'Starting...' : 'New Tab'}
                  </div>
                {:else}
                  {#each browserContext?.pages ?? [] as page (page.id)}
                    <div
                      class={cn('group flex h-9 max-w-44 shrink-0 items-center overflow-hidden rounded-t-md border border-b-0 text-xs', page.id === activeBrowserPage?.id ? 'border-zinc-700 bg-zinc-950 text-zinc-100' : 'border-zinc-800 bg-zinc-900/60 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200')}
                    >
                    <button
                      type="button"
                        class="min-w-0 flex-1 truncate px-3 text-left"
                      onclick={() => void handleBrowserSelectPage(page.id)}
                      title={page.title || page.url}
                    >
                        {browserTabLabel(page)}
                    </button>
                      <button
                        type="button"
                        class="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded text-zinc-500 opacity-70 hover:bg-zinc-800 hover:text-zinc-100 group-hover:opacity-100"
                        aria-label={`Close ${browserTabLabel(page)}`}
                        onclick={(event) => void handleBrowserClosePage(page.id, event)}
                      >
                        <X class="size-3.5" />
                      </button>
                    </div>
                  {/each}
                {/if}
                <Button type="button" variant="ghost" size="icon" class="mb-0.5 h-8 w-8 shrink-0 rounded-md" onclick={handleBrowserOpenTab} aria-label="New browser tab">
                  <Plus class="size-4" />
                </Button>
                <div class="sticky right-0 ml-auto flex shrink-0 items-center gap-1 self-stretch bg-zinc-950 pl-2">
                  <DropdownMenu.Root>
                    <DropdownMenu.Trigger
                      class="mb-0.5 inline-flex h-8 w-8 items-center justify-center rounded-md text-zinc-400 transition-colors hover:bg-zinc-900 hover:text-zinc-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-700 disabled:pointer-events-none disabled:opacity-50"
                      aria-label={`Viewport: ${browserViewportLabel(browserViewportMode)}`}
                      title={`Viewport: ${browserViewportLabel(browserViewportMode)}`}
                      disabled={!activeBrowserPage}
                    >
                      <MonitorSmartphone class="size-4" />
                    </DropdownMenu.Trigger>
                    <DropdownMenu.Content side="bottom" align="end" sideOffset={8} class="w-64">
                      <DropdownMenu.RadioGroup
                        value={browserViewportMode}
                        onValueChange={(value) => {
                          if (value === 'fit' || value === 'mobile' || value === 'desktop' || value === 'wide') {
                            void handleBrowserViewportMode(value);
                          }
                        }}
                      >
                        {#each BROWSER_VIEWPORT_MODES as mode}
                          <DropdownMenu.RadioItem value={mode} class="items-start gap-3 py-2 pl-2 pr-8">
                            <div class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-zinc-400">
                              <MonitorSmartphone class="size-4" />
                            </div>
                            <div class="min-w-0">
                              <div class="text-sm font-medium text-zinc-100">{browserViewportLabel(mode)}</div>
                              <div class="mt-0.5 text-xs leading-5 text-zinc-500">{browserViewportDescription(mode)}</div>
                            </div>
                          </DropdownMenu.RadioItem>
                        {/each}
                      </DropdownMenu.RadioGroup>
                    </DropdownMenu.Content>
                  </DropdownMenu.Root>
                  <Button
                    type="button"
                    variant={browserAnnotating ? 'default' : 'ghost'}
                    size="icon"
                    class="mb-0.5 h-8 w-8 rounded-md"
                    aria-label={browserAnnotating ? 'Stop annotating' : 'Annotate page'}
                    title={browserAnnotating ? 'Stop annotating' : 'Annotate page'}
                    onclick={() => (browserAnnotating = !browserAnnotating)}
                    disabled={!activeBrowserPage}
                  >
                    <NotebookPen class="size-4" />
                  </Button>
                </div>
              </div>
              <form class="flex gap-2" onsubmit={(event) => { event.preventDefault(); void handleBrowserNavigate(); }}>
                <Button type="button" variant="outline" size="icon" onclick={() => void handleBrowserCommand('back')} disabled={!activeBrowserPage}><ArrowLeft class="size-4" /></Button>
                <Button type="button" variant="outline" size="icon" onclick={() => void handleBrowserCommand('forward')} disabled={!activeBrowserPage}><ArrowRight class="size-4" /></Button>
                <Button type="button" variant="outline" size="icon" onclick={() => void handleBrowserCommand('reload')} disabled={!activeBrowserPage}><RotateCcw class="size-4" /></Button>
                <Input
                  bind:value={browserUrl}
                  aria-label="Browser URL"
                  placeholder="Search or enter address"
                  onfocus={() => {
                    browserUrlEditing = true;
                    if (browserUrl.trim() === 'about:blank') browserUrl = '';
                  }}
                  onblur={() => {
                    browserUrlEditing = false;
                    browserUrl = browserAddressValue(browserUrl);
                  }}
                />
              </form>
              {#if browserError}
                <div class="rounded-lg border border-red-900/60 bg-red-950/30 p-3 text-xs text-red-200">{browserError}</div>
              {/if}
              <div
                bind:this={browserStageElement}
                class={cn(
                  'relative min-h-0 flex-1 overflow-hidden',
                  browserViewportMode === 'mobile'
                    ? 'flex justify-center bg-zinc-950'
                    : 'rounded-xl border border-zinc-800 bg-black'
                )}
              >
                {#if browserSnapshot?.screenshot_data_url}
                  <button
                    type="button"
                    class={cn(
                      'relative overflow-hidden bg-black p-0 text-left focus:outline-none focus:ring-2 focus:ring-cyan-500',
                      browserViewportMode === 'mobile'
                        ? 'mx-auto flex h-full max-w-full rounded-xl border border-zinc-800'
                        : 'block h-full w-full',
                      browserAnnotating ? 'cursor-crosshair' : 'cursor-default'
                    )}
                    aria-label="Interactive browser viewport"
                    onmousedown={(event) => { event.currentTarget.focus(); sendBrowserPointer('pointer_down', event); }}
                    onmouseup={(event) => sendBrowserPointer('pointer_up', event)}
                    onmousemove={(event) => { if (event.buttons > 0) sendBrowserPointer('pointer_move', event); }}
                    onkeydown={handleBrowserKeydown}
                    onpaste={handleBrowserPaste}
                    onwheel={handleBrowserWheel}
                    oncontextmenu={(event) => event.preventDefault()}
                  >
                    <img
                      bind:this={browserViewportElement}
                      src={browserSnapshot.screenshot_data_url}
                      alt="Interactive browser viewport"
                      class={cn(
                        browserViewportMode === 'mobile'
                          ? 'h-full w-auto max-w-full object-contain'
                          : 'h-full w-full object-fill'
                      )}
                      draggable="false"
                    />
                    {#if browserLoading}
                      <div class="absolute right-3 top-3 rounded-full border border-zinc-700 bg-zinc-950/90 px-3 py-1 text-xs text-zinc-300">Loading…</div>
                    {/if}
                  </button>
                  {#if browserAnnotationDraft}
                    <div class="absolute bottom-3 left-3 right-3 z-10 max-w-md rounded-lg border border-zinc-700 bg-zinc-950/95 p-3 text-left shadow-xl">
                      <div class="mb-2 text-xs font-medium text-zinc-200">Browser annotation</div>
                      <Textarea
                        bind:value={browserAnnotationComment}
                        rows={3}
                        class="min-h-20 resize-none border-zinc-800 bg-zinc-900 text-sm text-zinc-100"
                        placeholder="Add a comment for this point..."
                        disabled={browserAnnotationSaving}
                      ></Textarea>
                      <div class="mt-3 flex justify-end gap-2">
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          disabled={browserAnnotationSaving}
                          onclick={() => {
                            browserAnnotationDraft = null;
                            browserAnnotationComment = '';
                          }}
                        >
                          Cancel
                        </Button>
                        <Button
                          type="button"
                          variant="secondary"
                          size="sm"
                          disabled={browserAnnotationSaving || browserAnnotationComment.trim().length === 0}
                          onclick={() => void saveBrowserAnnotation()}
                        >
                          {browserAnnotationSaving ? 'Saving' : 'Save'}
                        </Button>
                      </div>
                    </div>
                  {:else if browserAnnotation}
                    <div class="absolute bottom-3 left-3 z-10 max-w-[calc(100%-1.5rem)] rounded border border-cyan-700/50 bg-zinc-950/90 p-2 text-left text-[11px] text-cyan-100 shadow-xl">
                      Annotation saved
                    </div>
                  {/if}
                {:else if browserLoading}
                  <div class="flex h-full items-center justify-center text-sm text-zinc-500">Starting browser…</div>
                {:else}
                  <div class="flex h-full items-center justify-center p-6 text-center text-sm text-zinc-500">Enter a URL and press Enter to open the session browser. Click, type, and scroll directly in this viewport.</div>
                {/if}
              </div>
              </div>
            {:else}
              <div class="min-w-0 space-y-6 px-5 py-5">
              <section class="min-w-0 space-y-4">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Session</div>
                  <div class="text-sm text-zinc-400">
                    Keep metadata and destructive actions out of the main work surface.
                  </div>
                </div>

                <div class="block space-y-2">
                  <Label for="session-title" class="normal-case tracking-normal">Title</Label>
                  <Input
                    id="session-title"
                    bind:value={draftTitle}
                    placeholder="Session title"
                  />
                </div>

                <div class="block space-y-2">
                  <Label for="session-profile" class="normal-case tracking-normal">Profile</Label>
                  <Select
                    id="session-profile"
                    bind:value={draftProfileId}
                  >
                    {#if selectedSession.profile_id === ''}
                      <option value="">Legacy or direct target</option>
                    {/if}
                    {#each workspaceProfiles as profile}
                      <option value={profile.id}>{profile.title}</option>
                    {/each}
                  </Select>
                </div>

                <div class="block space-y-2">
                  <Label for="session-mode" class="normal-case tracking-normal">Session Mode</Label>
                  <Select
                    id="session-mode"
                    value={draftComposerMode()}
                    onchange={handleDraftComposerModeChange}
                  >
                    {#each COMPOSER_MODES as mode}
                      <option value={mode}>{composerModeLabel(mode)} - {composerModeDescription(mode)}</option>
                    {/each}
                  </Select>
                  <span class="block text-xs leading-5 text-zinc-500">
                    Choose whether Nucleus plans first, asks before actions, or auto-runs trusted actions.
                  </span>
                </div>

                <div class="block space-y-2">
                  <Label for="session-run-budget" class="normal-case tracking-normal">Run Budget</Label>
                  <Select
                    id="session-run-budget"
                    bind:value={draftRunBudgetMode}
                  >
                    {#each RUN_BUDGET_MODES as mode}
                      <option value={mode}>{runBudgetModeLabel(mode)} - {runBudgetModeDescription(mode)}</option>
                    {/each}
                  </Select>
                  <span class="block text-xs leading-5 text-zinc-500">
                    {runBudgetModeHelp(draftRunBudgetMode)}
                  </span>
                </div>

                <div class="grid gap-3 sm:grid-cols-2">
                  <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                    <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Provider</div>
                    <div class="mt-2 text-sm text-zinc-100">
                      {formatState(selectedSession.provider)}
                    </div>
                    <div class="mt-1 text-xs text-zinc-500">
                      {selectedSession.model || 'Provider default model'}
                    </div>
                  </div>

                  <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                    <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Workspace</div>
                    <div class="mt-2 break-all text-sm text-zinc-100">
                      {compactPath(selectedSession.working_dir)}
                    </div>
                    <div class="mt-1 text-xs text-zinc-500">
                      {formatState(selectedSession.workspace_mode)} · {formatState(selectedSession.working_dir_kind)}
                    </div>
                    {#if selectedSession.git_branch || selectedSession.git_head}
                      <div class="mt-2 text-xs text-zinc-400">
                        {selectedSession.git_branch || 'detached'}
                        {#if selectedSession.git_head}
                          · {selectedSession.git_head.slice(0, 12)}
                        {/if}
                        {#if selectedSession.git_dirty}
                          · dirty
                        {/if}
                      </div>
                    {/if}
                    {#if selectedSession.source_project_path && selectedSession.source_project_path !== selectedSession.working_dir}
                      <div class="mt-1 break-all text-xs text-zinc-500">Source: {compactPath(selectedSession.source_project_path)}</div>
                    {/if}
                  </div>
                </div>

                {#if selectedSession.workspace_warnings.length > 0}
                  <div class="rounded-xl border border-amber-700/60 bg-amber-950/30 px-3 py-3 text-sm text-amber-100">
                    <div class="text-[11px] uppercase tracking-[0.14em] text-amber-300">Workspace warning</div>
                    <ul class="mt-2 list-disc space-y-1 pl-4">
                      {#each selectedSession.workspace_warnings as warning}
                        <li>{warning}</li>
                      {/each}
                    </ul>
                  </div>
                {/if}

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Routing</div>
                  <div class="mt-2 break-words text-sm text-zinc-100">
                    {selectedSession.profile_title ||
                      selectedSession.route_title ||
                      'Direct session target'}
                  </div>
                  <div class="mt-1 break-all text-xs text-zinc-500">
                    {#if selectedProfile}
                      {selectedProfile.main.adapter === 'openai_compatible'
                        ? selectedProfile.main.base_url || 'OpenAI-compatible endpoint'
                        : `${formatState(selectedProfile.main.adapter)} runtime`}
                    {:else}
                      {selectedSession.route_id || selectedSession.provider}
                    {/if}
                  </div>
                </div>

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Provider Thread</div>
                  <div class="mt-2 break-all text-sm text-zinc-100">
                    {selectedSession.provider_session_id || 'Waiting for first successful turn'}
                  </div>
                </div>

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Session Mode</div>
                  <div class="mt-2 text-sm text-zinc-100">
                    {composerModeLabel(sessionComposerMode(selectedSession))}
                  </div>
                  <div class="mt-1 text-xs text-zinc-500">
                    {composerModeDescription(sessionComposerMode(selectedSession))}
                  </div>
                </div>

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Run Budget</div>
                  <div class="mt-2 text-sm text-zinc-100">
                    {runBudgetModeLabel(normalizeRunBudgetMode(selectedSession.run_budget_mode))}
                  </div>
                  <div class="mt-1 text-xs text-zinc-500">
                    {formatRunBudget(selectedSession)}
                  </div>
                </div>

                <div class="flex flex-wrap gap-2">
                  <Button
                    variant="secondary"
                    disabled={!sessionSettingsDirty || savingSession}
                    onclick={handleSaveSessionSettings}
                  >
                    <Save class="size-4" />
                    <span>{savingSession ? 'Saving' : 'Save'}</span>
                  </Button>

                  <Button variant="outline" disabled={actioning} onclick={handleArchiveToggle}>
                    <Archive class="size-4" />
                    <span>{selectedSession.state === 'archived' ? 'Restore' : 'Archive'}</span>
                  </Button>

                  <Button variant="destructive" disabled={actioning} onclick={handleDeleteSession}>
                    <Trash2 class="size-4" />
                    <span>
                      {deleteConfirmId === selectedSession.id ? 'Confirm Delete' : 'Delete'}
                    </span>
                  </Button>
                </div>
              </section>

              <section class="min-w-0 space-y-4 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Projects</div>
                  <div class="text-sm text-zinc-400">
                    Attach, detach, or promote workspace projects without changing the main workspace root.
                  </div>
                </div>

                <div class="min-w-0 rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Primary Context</div>
                  <div class="mt-2 truncate text-sm text-zinc-100">{selectedProjectTitle}</div>
                  {#if attachedProjects.length === 0}
                    <div class="mt-1 text-xs text-zinc-500">
                      This session is currently running from workspace scratch.
                    </div>
                  {/if}
                </div>

                <div class="min-w-0 space-y-3">
                  {#each workspaceProjects as project}
                    <div class="min-w-0 rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                      <div class="flex items-start justify-between gap-3">
                        <div class="min-w-0 flex-1">
                          <div class="truncate text-sm font-medium text-zinc-100">{project.title}</div>
                          <div class="mt-1 truncate text-xs text-zinc-500">
                            {compactPath(project.absolute_path)}
                          </div>
                        </div>
                        <div class="flex shrink-0 flex-wrap gap-1">
                          {#if attachedProjects.some((attached) => attached.id === project.id && attached.is_primary)}
                            <Badge variant="default">Primary</Badge>
                          {:else if attachedProjects.some((attached) => attached.id === project.id)}
                            <Badge variant="secondary">Attached</Badge>
                          {/if}
                        </div>
                      </div>

                      <div class="mt-3 flex flex-wrap gap-2">
                        {#if attachedProjects.some((attached) => attached.id === project.id)}
                          {#if !attachedProjects.some((attached) => attached.id === project.id && attached.is_primary)}
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={updatingProjectId === project.id}
                              onclick={() => handleProjectAction(project.id, 'primary')}
                            >
                              Make Primary
                            </Button>
                          {/if}
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={updatingProjectId === project.id}
                            onclick={() => handleProjectAction(project.id, 'detach')}
                          >
                            Detach
                          </Button>
                        {:else}
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={updatingProjectId === project.id}
                            onclick={() => handleProjectAction(project.id, 'attach')}
                          >
                            Attach
                          </Button>
                        {/if}
                      </div>
                    </div>
                  {/each}
                </div>
              </section>

              <section class="space-y-4 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Utility Worker Jobs</div>
                  <div class="text-sm text-zinc-400">
                    The activity drawer shows live Nucleus activity. Full Utility Worker history stays here.
                  </div>
                </div>

                {#if jobLoading && jobSummaries.length === 0}
                  <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-4 text-sm text-zinc-500">
                    Loading Nucleus job history...
                  </div>
                {:else if jobSummaries.length === 0}
                  <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-4 text-sm text-zinc-500">
                    No Utility Worker jobs have been recorded for this session yet.
                  </div>
                {:else}
                  <div class="space-y-3">
                    {#each jobSummaries.slice(0, 4) as job}
                      <button
                        type="button"
                        class={cn(
                          'w-full rounded-xl border px-3 py-3 text-left transition-colors',
                          selectedJobId === job.id
                            ? 'border-lime-300/35 bg-lime-300/8'
                            : 'border-zinc-800 bg-zinc-900/75 hover:border-zinc-700'
                        )}
                        onclick={() => {
                          void loadJob(job.id);
                        }}
                      >
                        <div class="flex items-start justify-between gap-3">
                          <div class="min-w-0">
                            <div class="truncate text-sm font-medium text-zinc-100">{job.title}</div>
                            <div class="mt-1 text-xs text-zinc-500">{job.prompt_excerpt || job.purpose}</div>
                          </div>
                          <Badge variant={badgeVariantForJobState(job.state)}>
                            {jobCompletionLabel(job)}
                          </Badge>
                        </div>
                        <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-zinc-600">
                          <span>{job.worker_count} Utility Worker{job.worker_count === 1 ? '' : 's'}</span>
                          <span>{job.pending_approval_count} approvals</span>
                          <span>{job.artifact_count} artifacts</span>
                          <span>{formatDateTime(job.updated_at)}</span>
                        </div>
                      </button>
                    {/each}
                  </div>

                  {#if jobDetail}
                    <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                      <div class="flex items-start justify-between gap-3">
                        <div class="min-w-0">
                          <div class="truncate text-sm font-medium text-zinc-100">{jobDetail.job.title}</div>
                          <div class="mt-1 text-xs leading-5 text-zinc-500">
                            {jobDetail.job.result_summary || jobDetail.job.prompt_excerpt || jobDetail.job.purpose}
                          </div>
                        </div>
                        <Badge variant={badgeVariantForJobState(jobDetail.job.state)}>
                          {jobCompletionLabel(jobDetail.job)}
                        </Badge>
                      </div>

                      {#if jobDetail.job.user_error}
                        <FriendlyErrorNotice
                          userError={jobDetail.job.user_error}
                          class="mt-3"
                          onRetryJob={() => void handleResumeJob(jobDetail?.job.id)}
                          onCancelJob={() => void handleCancelJob(jobDetail?.job.id)}
                          retryDisabled={jobActioning}
                          cancelDisabled={jobActioning}
                        />
                      {:else if jobDetail.job.last_error}
                        <div class="mt-3 rounded-lg border border-red-500/20 bg-red-500/10 px-3 py-2 text-xs text-red-200">
                          {jobDetail.job.last_error}
                        </div>
                      {/if}

                      {#if jobDetail.job.browser_verification_required || jobDetail.job.browser_verification_status !== 'not_required'}
                        <div class="mt-3 rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-3">
                          <div class="flex items-start justify-between gap-3">
                            <div class="min-w-0">
                              <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Browser Verification</div>
                              <div class="mt-1 text-sm text-zinc-100">
                                {formatVerificationStatus(jobDetail.job.browser_verification_status)}
                              </div>
                              {#if jobDetail.job.browser_verification_summary}
                                <div class="mt-1 text-xs leading-5 text-zinc-500">
                                  {jobDetail.job.browser_verification_summary}
                                </div>
                              {/if}
                            </div>
                            <Badge variant={badgeVariantForVerification(jobDetail.job.browser_verification_status)}>
                              {jobDetail.job.browser_verification_required ? 'Required' : 'Optional'}
                            </Badge>
                          </div>
                          {#if jobDetail.job.browser_verification_artifact_ids.length > 0}
                            <div class="mt-3 flex flex-wrap gap-1.5">
                              {#each jobDetail.job.browser_verification_artifact_ids as artifactId}
                                <span class="rounded border border-zinc-800 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-500">{artifactId}</span>
                              {/each}
                            </div>
                          {/if}
                        </div>
                      {/if}

                      <div class="mt-3 flex flex-wrap gap-2">
                        {#if jobDetail.job.state === 'running' || jobDetail.job.state === 'queued'}
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={jobActioning}
                            onclick={() => handleCancelJob()}
                          >
                            <XCircle class="size-4" />
                            <span>{jobActioning ? 'Stopping' : 'Cancel Job'}</span>
                          </Button>
                        {/if}

                        {#if (jobDetail.job.state === 'paused' || jobDetail.job.state === 'failed') && !selectedJobHasPendingApprovals}
                          <Button
                            variant="secondary"
                            size="sm"
                            disabled={jobActioning}
                            onclick={() => handleResumeJob()}
                          >
                            <RotateCcw class={cn('size-4', jobActioning && 'animate-spin')} />
                            <span>{jobActioning ? 'Retrying' : jobDetail.job.state === 'failed' ? 'Retry Job' : 'Resume Job'}</span>
                          </Button>
                        {/if}
                      </div>

                      <div class="mt-4 space-y-3 border-t border-zinc-800 pt-4">
                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Subtasks</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.child_jobs.length === 0}
                              <div class="text-xs text-zinc-500">No subtasks were recorded for this job.</div>
                            {:else}
                              {#each jobDetail.child_jobs as childJob}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{childJob.title}</div>
                                      <div class="mt-1 text-xs leading-5 text-zinc-500">
                                        {childJob.purpose}
                                        {#if childJob.result_summary}
                                          {' · '}{childJob.result_summary}
                                        {/if}
                                      </div>
                                    </div>
                                    <Badge variant={badgeVariantForJobState(childJob.state)}>
                                      {formatState(childJob.state)}
                                    </Badge>
                                  </div>
                                  <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-zinc-600">
                                    <span>{childJob.worker_count} Utility Worker{childJob.worker_count === 1 ? '' : 's'}</span>
                                    <span>{childJob.artifact_count} artifact{childJob.artifact_count === 1 ? '' : 's'}</span>
                                    {#if childJob.updated_at}
                                      <span>Updated {formatDateTime(childJob.updated_at)}</span>
                                    {/if}
                                  </div>
                                  {#if childJob.user_error}
                                    <FriendlyErrorNotice userError={childJob.user_error} class="mt-2" />
                                  {:else if childJob.last_error}
                                    <div class="mt-2 text-xs leading-5 text-red-200">{childJob.last_error}</div>
                                  {/if}
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Utility Workers</div>
                          <div class="mt-2 space-y-2">
                            {#each jobDetail.workers as worker}
                              <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                <div class="flex items-center justify-between gap-3">
                                  <div class="min-w-0">
                                    <div class="truncate text-sm text-zinc-100">{worker.title}</div>
                                    <div class="mt-1 text-xs text-zinc-500">
                                      {formatWorkerSummary(worker)}
                                    </div>
                                  </div>
                                  <Badge variant={badgeVariantForJobState(worker.state)}>
                                    {formatState(worker.state)}
                                  </Badge>
                                </div>
                                <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-zinc-600">
                                  <span>{worker.step_count}/{worker.max_steps} steps</span>
                                  <span>{worker.tool_call_count}/{worker.max_tool_calls} actions</span>
                                  <span>{compactPath(worker.working_dir)}</span>
                                </div>
                                {#if worker.user_error}
                                  <FriendlyErrorNotice userError={worker.user_error} class="mt-2" />
                                {:else if worker.last_error}
                                  <div class="mt-2 text-xs leading-5 text-red-200">{worker.last_error}</div>
                                {/if}
                              </div>
                            {/each}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Actions</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.tool_calls.length === 0}
                              <div class="text-xs text-zinc-500">No actions were recorded for this job yet.</div>
                            {:else}
                              {#each [...jobDetail.tool_calls].reverse().slice(0, 6) as toolCall}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-center justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{formatToolCallTitle(toolCall)}</div>
                                      <div class="mt-1 text-xs text-zinc-500">{formatToolCallTiming(toolCall)}</div>
                                      {#if formatToolCallSummary(toolCall) !== formatToolCallTitle(toolCall)}
                                        <div class="mt-1 text-xs leading-5 text-zinc-500">
                                          {compactText(formatToolCallSummary(toolCall), 240)}
                                        </div>
                                      {/if}
                                    </div>
                                    <Badge variant={badgeVariantForToolCall(toolCall.status)}>
                                      {formatState(toolCall.status)}
                                    </Badge>
                                  </div>
                                  {#if toolCall.tool_id === 'command.run'}
                                    <details class="mt-2 text-xs leading-5 text-zinc-500">
                                      <summary class="cursor-pointer select-none text-zinc-400">Command details</summary>
                                      <pre class="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-[11px] leading-5 text-zinc-400">{formatToolCallCommandDetail(toolCall)}</pre>
                                    </details>
                                  {/if}
                                  {#if toolCall.error_detail}
                                    <div class="mt-2 text-xs leading-5 text-red-200">{toolCall.error_detail}</div>
                                  {:else if toolCall.result_json}
                                    <pre class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{formatJsonPreview(toolCall.result_json)}</pre>
                                  {/if}
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Approvals</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.approvals.length === 0}
                              <div class="text-xs text-zinc-500">No approval requests were recorded for this job.</div>
                            {:else}
                              {#each [...jobDetail.approvals].reverse().slice(0, 6) as approval}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{formatApprovalTitle(approval, jobDetail.tool_calls)}</div>
                                      <div class="mt-1 text-xs leading-5 text-zinc-500">{formatApprovalDetail(approval, jobDetail.tool_calls)}</div>
                                    </div>
                                    <Badge variant={badgeVariantForJobState(approval.state)}>
                                      {formatState(approval.state)}
                                    </Badge>
                                  </div>
                                  {#if approval.diff_preview}
                                    <pre class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{approval.diff_preview}</pre>
                                  {/if}
                                  {#if approval.resolution_note}
                                    <div class="mt-2 text-xs leading-5 text-zinc-500">{approval.resolution_note}</div>
                                  {/if}
                                  {#if approval.state === 'pending'}
                                    <div class="mt-3 flex flex-wrap gap-2">
                                      <Button
                                        variant="secondary"
                                        size="sm"
                                        disabled={approvalActioningId !== null}
                                        onclick={() => {
                                          void handleApproveRequest(approval);
                                        }}
                                      >
                                        <span>{approvalActioningId === approval.id ? 'Approving' : 'Approve'}</span>
                                      </Button>
                                      <Button
                                        variant="outline"
                                        size="sm"
                                        disabled={approvalActioningId !== null}
                                        onclick={() => {
                                          void handleDenyRequest(approval);
                                        }}
                                      >
                                        <span>{approvalActioningId === approval.id ? 'Resolving' : 'Deny'}</span>
                                      </Button>
                                    </div>
                                  {/if}
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Command Sessions</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.command_sessions.length === 0}
                              <div class="text-xs text-zinc-500">No command sessions were recorded for this job.</div>
                            {:else}
                              {#each [...jobDetail.command_sessions].reverse().slice(0, 6) as commandSession}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{formatCommandSessionSummary(commandSession)}</div>
                                      <div class="mt-1 text-xs leading-5 text-zinc-500">
                                        {commandSession.command}
                                        {#if commandSession.args.length > 0}
                                          {' '}{commandSession.args.join(' ')}
                                        {/if}
                                      </div>
                                    </div>
                                    <Badge variant={badgeVariantForToolCall(commandSession.state)}>
                                      {formatState(commandSession.state)}
                                    </Badge>
                                  </div>
                                  <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-zinc-600">
                                    <span>{commandSession.mode}</span>
                                    <span>{compactPath(commandSession.cwd)}</span>
                                    <span>{commandSession.output_limit_bytes} byte cap</span>
                                    <span>{commandSession.timeout_secs}s timeout</span>
                                  </div>
                                  {#if commandSession.last_error}
                                    <div class="mt-2 text-xs leading-5 text-red-200">{commandSession.last_error}</div>
                                  {/if}
                                  <div class="mt-2 text-[11px] text-zinc-600">
                                    {#if commandSession.started_at}
                                      Started {formatDateTime(commandSession.started_at)}
                                    {/if}
                                    {#if commandSession.completed_at}
                                      {' · '}Completed {formatDateTime(commandSession.completed_at)}
                                    {/if}
                                  </div>
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Artifacts</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.artifacts.length === 0}
                              <div class="text-xs text-zinc-500">No artifacts were recorded for this job yet.</div>
                            {:else}
                              {#each [...jobDetail.artifacts].reverse().slice(0, 6) as artifact}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{formatArtifactSummary(artifact)}</div>
                                      <div class="mt-1 text-xs text-zinc-500">{artifact.kind} · {formatDateTime(artifact.created_at)}</div>
                                    </div>
                                    <div class="shrink-0 text-[11px] text-zinc-600">{artifact.size_bytes} bytes</div>
                                  </div>
                                  {#if artifact.preview_text}
                                    <pre class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{artifact.preview_text}</pre>
                                  {/if}
                                  <div class="mt-2 text-[11px] text-zinc-600">{compactPath(artifact.path)}</div>
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>

                        <div>
                          <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Timeline</div>
                          <div class="mt-2 space-y-2">
                            {#if jobDetail.events.length === 0}
                              <div class="text-xs text-zinc-500">No job events have been recorded yet.</div>
                            {:else}
                              {#each [...jobDetail.events].reverse().slice(0, 8) as event}
                                <div class="rounded-lg border border-zinc-800 bg-zinc-950/70 px-3 py-2">
                                  <div class="flex items-start justify-between gap-3">
                                    <div class="min-w-0">
                                      <div class="truncate text-sm text-zinc-100">{formatJobEvent(event)}</div>
                                      {#if event.detail}
                                        <div class="mt-1 text-xs leading-5 text-zinc-500">{event.detail}</div>
                                      {/if}
                                    </div>
                                    <Badge variant={badgeVariantForJobState(event.status || 'queued')}>
                                      {formatState(event.status || event.event_type)}
                                    </Badge>
                                  </div>
                                  <div class="mt-2 text-[11px] text-zinc-600">{formatDateTime(event.created_at)}</div>
                                </div>
                              {/each}
                            {/if}
                          </div>
                        </div>
                      </div>
                    </div>
                  {/if}
                {/if}
              </section>

              <section class="space-y-4 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Actions</div>
                  <div class="text-sm text-zinc-400">
                    Operational actions stay available, but they do not need to crowd the transcript.
                  </div>
                </div>

                <div class="space-y-3">
                  {#each actions as action}
                    <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                      <div class="flex items-start justify-between gap-3">
                        <div class="min-w-0">
                          <div class="flex flex-wrap items-center gap-2">
                            <div class="text-sm font-medium text-zinc-100">{action.title}</div>
                            <Badge variant={badgeVariantForActionRisk(action.risk)}>
                              {formatState(action.risk)}
                            </Badge>
                          </div>
                          <div class="mt-1 text-xs leading-5 text-zinc-500">{action.summary}</div>
                        </div>
                        <div class="shrink-0 text-[11px] uppercase tracking-[0.14em] text-zinc-600">
                          {action.category}
                        </div>
                      </div>

                      {#if action.parameters.length > 0}
                        <div class="mt-3 space-y-3">
                          {#each action.parameters as parameter}
                            <div class="block space-y-1">
                              <Label
                                for={`action-${action.id}-${parameter.name}`}
                                class="normal-case tracking-normal"
                              >
                                {parameter.label}
                              </Label>
                              <Input
                                id={`action-${action.id}-${parameter.name}`}
                                class="h-9"
                                value={actionFormValues[action.id]?.[parameter.name] ?? ''}
                                placeholder={parameter.default_value || parameter.description}
                                oninput={(event) =>
                                  setActionFormValue(
                                    action.id,
                                    parameter.name,
                                    (event.currentTarget as HTMLInputElement).value
                                  )}
                              />
                              {#if parameter.description}
                                <div class="text-[11px] text-zinc-600">{parameter.description}</div>
                              {/if}
                            </div>
                          {/each}
                        </div>
                      {/if}

                      <div class="mt-3 flex flex-wrap gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={actionRunningId === action.id}
                          onclick={() => handleRunAction(action)}
                        >
                          {#if action.id === 'runtime.refresh'}
                            <RotateCcw class={cn('size-4', actionRunningId === action.id && 'animate-spin')} />
                          {:else if action.id === 'workspace.sync'}
                            <Router class={cn('size-4', actionRunningId === action.id && 'animate-spin')} />
                          {:else}
                            <Wrench class="size-4" />
                          {/if}
                          <span>
                            {actionConfirmId === action.id
                              ? 'Confirm'
                              : actionRunningId === action.id
                                ? 'Running'
                                : 'Run'}
                          </span>
                        </Button>
                      </div>
                    </div>
                  {/each}
                </div>
              </section>

              <section class="space-y-4 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Recent Activity</div>
                  <div class="text-sm text-zinc-400">
                    Audit history stays live from the Nucleus stream, without taking over the session page.
                  </div>
                </div>

                <div class="space-y-3">
                  {#each auditEvents as event}
                    <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                      <div class="flex items-center justify-between gap-3">
                        <div class="truncate text-sm font-medium text-zinc-100">{event.summary}</div>
                        <Badge variant={badgeVariantForAuditStatus(event.status)}>
                          {formatState(event.status)}
                        </Badge>
                      </div>
                      <div class="mt-2 text-xs leading-5 text-zinc-500">{event.detail}</div>
                      <div class="mt-2 text-[11px] text-zinc-600">{formatDateTime(event.created_at)}</div>
                    </div>
                  {/each}
                </div>
              </section>
            </div>
            {/if}
          </SheetContent>
        </Sheet>
      </div>
    {/if}
  </div>
</div>
