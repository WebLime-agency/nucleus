<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount, tick } from 'svelte';
  import {
    Archive,
    Bot,
    FolderTree,
    ImagePlus,
    MessageSquare,
    PanelRightClose,
    PanelRightOpen,
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

  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    deleteSession,
    fetchActions,
    fetchAuditEvents,
    fetchOverview,
    fetchSessionDetail,
    runAction,
    sendSessionPrompt,
    updateSession
  } from '$lib/nucleus/client';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import { connectDaemonStream, type StreamStatus } from '$lib/nucleus/realtime';
  import type {
    ActionSummary,
    AuditEvent,
    DaemonEvent,
    PromptProgressUpdate,
    RuntimeOverview,
    SessionDetail,
    SessionSummary,
    SessionTurn,
    SessionTurnImage
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
  let actionFormValues = $state<Record<string, Record<string, string>>>({});
  let detailPanelOpen = $state(false);
  let dragOver = $state(false);
  let promptImages = $state<ComposerImage[]>([]);
  let promptProgress = $state<PromptProgressUpdate[]>([]);
  let transcriptAnchor = $state('');

  let transcriptElement = $state<HTMLDivElement | null>(null);
  let fileInputElement = $state<HTMLInputElement | null>(null);

  let sessions = $derived(overview?.sessions ?? []);
  let routerProfiles = $derived(overview?.router_profiles ?? []);
  let workspace = $derived(overview?.workspace ?? null);
  let workspaceProjects = $derived(workspace?.projects ?? []);
  let requestedSessionId = $derived(page.url.searchParams.get('session') ?? '');
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
  let attachedProjects = $derived(selectedSession?.projects ?? []);
  let selectedProject = $derived(attachedProjects.find((project) => project.is_primary) ?? null);
  let selectedProjectTitle = $derived(
    selectedProject?.title ??
      selectedSession?.project_title ??
      (selectedSession?.project_count === 0 ? 'Workspace scratch' : 'No primary project')
  );
  let sessionSettingsDirty = $derived(
    selectedSession
      ? draftTitle !== selectedSession.title || draftProfileId !== selectedSession.profile_id
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
  let activePromptProgress = $derived(promptProgress[promptProgress.length - 1] ?? null);

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
    if (state === 'archived') return 'secondary';
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
    detail = next;
    selectedSessionId = next.session.id;
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
      turn_count: session.turn_count + 1,
      updated_at: now,
      last_message_excerpt: prompt.trim()
    });

    promptProgress = [
      {
        session_id: session.id,
        status: 'queued',
        label: 'Sending to daemon',
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

  async function loadSelectedSession(sessionId: string, silent = false) {
    if (!sessionId) {
      selectedSessionId = '';
      detail = null;
      setSessionDrafts(null);
      return;
    }

    const previousId = selectedSessionId;
    selectedSessionId = sessionId;
    sessionRequestInFlight = sessionId;

    if (previousId !== sessionId) {
      clearComposerState();
      promptProgress = [];
    }

    if (!silent) {
      sessionLoading = true;
    }

    try {
      detail = await fetchSessionDetail(sessionId);
      setSessionDrafts(detail.session);
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
        profile_id: draftProfileId || undefined
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

  async function handlePromptSubmit() {
    if (!selectedSession || !promptReady || selectedSession.state === 'running') {
      return;
    }

    const submittedSession = selectedSession;
    const submittedPrompt = promptText;
    const submittedImages = [...promptImages];

    sending = true;
    deleteConfirmId = null;
    actionConfirmId = null;
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

      await goto(fallbackId ? `/sessions?session=${fallbackId}` : '/sessions', {
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
    await goto(`/sessions?session=${sessionId}`, { noScroll: true });
  }

  function applyStreamEvent(event: DaemonEvent) {
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

<div class="-my-6 flex h-full min-h-0 overflow-hidden">
  <div class="flex min-w-0 flex-1 overflow-hidden border-y border-zinc-900 bg-zinc-950/70 lg:border-x">
    {#if loading && sessions.length === 0}
      <div class="flex flex-1 items-center justify-center px-8">
        <div class="max-w-md text-center">
          <div class="inline-flex h-12 w-12 items-center justify-center rounded-full border border-zinc-800 bg-zinc-900/80">
            <RotateCcw class="size-5 animate-spin text-zinc-400" />
          </div>
          <div class="mt-4 text-lg font-medium text-zinc-100">Connecting to the daemon</div>
          <div class="mt-2 text-sm text-zinc-500">
            Nucleus is loading sessions, workspace state, and route readiness.
          </div>
        </div>
      </div>
    {:else if !selectedSession}
      <div class="flex flex-1 items-center justify-center px-8">
        <div class="max-w-lg text-center">
          <div class="inline-flex h-14 w-14 items-center justify-center rounded-full border border-zinc-800 bg-zinc-900/80">
            <MessageSquare class="size-6 text-zinc-400" />
          </div>
          <div class="mt-4 text-lg font-medium text-zinc-100">Select or start a session</div>
          <div class="mt-2 text-sm leading-6 text-zinc-500">
            Session history stays in the sidebar. Open one there and the full work surface stays here.
          </div>
          <div class="mt-4 inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/80 px-3 py-1.5 text-xs text-zinc-400">
            <span>{statusLabel}</span>
            <span class="text-zinc-700">/</span>
            <span>{sessions.length} sessions</span>
          </div>
        </div>
      </div>
    {:else}
      <div class="relative flex min-w-0 flex-1 overflow-hidden">
        <div class="flex min-w-0 flex-1 flex-col overflow-hidden">
          <header class="border-b border-zinc-900 bg-zinc-950/90 px-5 py-4 sm:px-6">
            <div class="flex flex-wrap items-start justify-between gap-4">
              <div class="min-w-0">
                <div class="flex flex-wrap items-center gap-2">
                  <div class="truncate text-lg font-semibold text-zinc-50">{selectedSession.title}</div>
                  <Badge variant={badgeVariantForSession(selectedSession.state)}>
                    {formatState(selectedSession.state)}
                  </Badge>
                  {#if selectedSession.provider_session_id}
                    <Badge variant="secondary">Provider thread linked</Badge>
                  {/if}
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
                    <span>{selectedProjectTitle}</span>
                  </span>
                  <span>{selectedSession.turn_count} turns</span>
                  <span>{formatDateTime(selectedSession.updated_at)}</span>
                </div>
              </div>

              <div class="flex items-center gap-2">
                {#if sessionLoading}
                  <div class="inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/70 px-3 py-1 text-xs text-zinc-500">
                    <RotateCcw class="size-3.5 animate-spin" />
                    Loading
                  </div>
                {/if}
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={detailPanelOpen ? 'Close session details' : 'Open session details'}
                  onclick={() => {
                    detailPanelOpen = !detailPanelOpen;
                  }}
                >
                  {#if detailPanelOpen}
                    <PanelRightClose class="size-4" />
                  {:else}
                    <PanelRightOpen class="size-4" />
                  {/if}
                </Button>
              </div>
            </div>
          </header>

          {#if error || actionResultMessage || selectedSession.last_error}
            <div class="border-b border-zinc-900 bg-zinc-950/75 px-5 py-3 sm:px-6">
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

              {#if !error && !actionResultMessage && selectedSession.last_error}
                <div class="rounded-lg border border-red-500/25 bg-red-500/10 px-3 py-2 text-sm text-red-200">
                  {selectedSession.last_error}
                </div>
              {/if}
            </div>
          {/if}

          <div bind:this={transcriptElement} class="min-h-0 flex-1 overflow-y-auto px-5 py-6 sm:px-6">
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
                          <div class="break-words whitespace-pre-wrap text-sm leading-6">
                            {turn.content}
                          </div>
                        {/if}
                      </div>
                    </div>
                  </div>
                {/each}

                {#if selectedSession.state === 'running' || activePromptProgress}
                  <div class="flex justify-start">
                    <div class="flex max-w-3xl flex-col gap-2 items-start">
                      <div class="flex items-center gap-2 text-xs text-zinc-500">
                        <span>Nucleus</span>
                        <span class="text-zinc-700">/</span>
                        <span>{activePromptProgress ? formatPromptProgressStatus(activePromptProgress.status) : 'Working'}</span>
                      </div>
                      <div class="rounded-2xl border border-zinc-800 bg-zinc-900/85 px-4 py-3 shadow-sm">
                        <div class="flex flex-wrap items-center gap-2">
                          <div class="inline-flex h-7 w-7 items-center justify-center rounded-full border border-zinc-700 bg-zinc-950">
                            <Bot class="size-3.5 text-zinc-300" />
                          </div>
                          <div class="text-sm font-medium text-zinc-100">
                            {activePromptProgress?.label ?? 'Working on your prompt'}
                          </div>
                          <Badge
                            variant={badgeVariantForPromptStatus(activePromptProgress?.status ?? 'queued')}
                          >
                            {formatPromptProgressStatus(activePromptProgress?.status ?? 'queued')}
                          </Badge>
                        </div>

                        <div class="mt-2 text-sm leading-6 text-zinc-400">
                          {activePromptProgress?.detail ?? 'The daemon is preparing the next turn.'}
                        </div>

                        {#if promptProgress.length > 1}
                          <div class="mt-3 space-y-2 border-t border-zinc-800 pt-3">
                            {#each promptProgress as step, index (index)}
                              <div class="flex items-start justify-between gap-3 text-xs">
                                <div class="min-w-0">
                                  <div class="text-zinc-300">{step.label}</div>
                                  {#if step.detail}
                                    <div class="mt-0.5 text-zinc-500">{step.detail}</div>
                                  {/if}
                                </div>
                                <div class="shrink-0 text-zinc-600">
                                  {step.attempt_count > 0 ? `${step.attempt}/${step.attempt_count}` : ''}
                                </div>
                              </div>
                            {/each}
                          </div>
                        {/if}
                      </div>
                    </div>
                  </div>
                {/if}
              </div>
            {:else}
              <div class="flex h-full min-h-[22rem] items-center justify-center">
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

          <div class="border-t border-zinc-900 bg-zinc-950/92 px-5 py-4 sm:px-6">
            {#if promptImages.length > 0}
              <div class="mb-3 flex gap-3 overflow-x-auto pb-1">
                {#each promptImages as image}
                  <div class="relative w-28 shrink-0 overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900/85">
                    <img
                      src={image.data_url}
                      alt={image.display_name}
                      class="aspect-square w-full object-cover"
                    />
                    <button
                      type="button"
                      class="absolute right-2 top-2 inline-flex h-6 w-6 items-center justify-center rounded-full bg-black/75 text-zinc-100"
                      aria-label={`Remove ${image.display_name}`}
                      onclick={() => removeImage(image.id)}
                    >
                      <X class="size-3.5" />
                    </button>
                    <div class="truncate border-t border-zinc-800 px-2 py-2 text-[11px] text-zinc-400">
                      {image.display_name}
                    </div>
                  </div>
                {/each}
              </div>
            {/if}

            <div
              role="group"
              aria-label="Session composer"
              class={cn(
                'rounded-2xl border bg-zinc-900/80 p-3 transition-colors',
                dragOver ? 'border-lime-300/50 bg-lime-300/8' : 'border-zinc-800'
              )}
              ondragover={handleComposerDragOver}
              ondragleave={handleComposerDragLeave}
              ondrop={handleComposerDrop}
            >
              <textarea
                bind:value={promptText}
                class="min-h-[7.5rem] w-full resize-none bg-transparent text-sm leading-6 text-zinc-100 outline-none placeholder:text-zinc-500"
                placeholder="Send a message..."
                spellcheck={false}
                disabled={sending || selectedSession.state === 'archived'}
                onkeydown={handleComposerKeydown}
                onpaste={handleComposerPaste}
              ></textarea>

              <div class="mt-3 flex flex-wrap items-center justify-between gap-3 border-t border-zinc-800 pt-3">
                <div class="min-w-0">
                  <div class="text-xs text-zinc-400">{composerHint}</div>
                  <div class="mt-1 text-[11px] text-zinc-600">
                    Enter sends. Shift+Enter adds a new line.
                  </div>
                </div>

                <div class="flex items-center gap-2">
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
                    aria-label="Attach image"
                    disabled={
                      !sessionSupportsImages ||
                      sending ||
                      selectedSession.state === 'archived' ||
                      selectedSession.state === 'running'
                    }
                    onclick={triggerImagePicker}
                  >
                    <ImagePlus class="size-4" />
                  </Button>
                  <Button
                    variant="default"
                    disabled={
                      !promptReady ||
                      sending ||
                      selectedSession.state === 'archived' ||
                      selectedSession.state === 'running'
                    }
                    onclick={handlePromptSubmit}
                  >
                    <Send class={cn('size-4', sending && 'animate-pulse')} />
                    <span>{sending ? 'Handing Off' : 'Send'}</span>
                  </Button>
                </div>
              </div>
            </div>
          </div>
        </div>

        {#if detailPanelOpen}
          <button
            type="button"
            class="fixed inset-0 z-10 bg-black/50 lg:hidden"
            aria-label="Close session details"
            onclick={() => {
              detailPanelOpen = false;
            }}
          ></button>

          <aside class="fixed inset-y-0 right-0 z-20 flex w-full max-w-md flex-col overflow-y-auto border-l border-zinc-900 bg-zinc-950 lg:static lg:z-auto">
            <div class="flex items-center justify-between border-b border-zinc-900 px-5 py-4">
              <div>
                <div class="text-sm font-medium text-zinc-100">Session Details</div>
                <div class="mt-1 text-xs text-zinc-500">Secondary controls live here so the chat stays clear.</div>
              </div>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Close details"
                onclick={() => {
                  detailPanelOpen = false;
                }}
              >
                <X class="size-4" />
              </Button>
            </div>

            <div class="space-y-6 px-5 py-5">
              <section class="space-y-4">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Session</div>
                  <div class="text-sm text-zinc-400">
                    Keep metadata and destructive actions out of the main work surface.
                  </div>
                </div>

                <label class="block space-y-2">
                  <span class="text-xs text-zinc-500">Title</span>
                  <input
                    bind:value={draftTitle}
                    class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                    placeholder="Session title"
                  />
                </label>

                <label class="block space-y-2">
                  <span class="text-xs text-zinc-500">Profile</span>
                  <select
                    bind:value={draftProfileId}
                    class="h-10 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
                  >
                    {#if selectedSession.profile_id === ''}
                      <option value="">Legacy or direct target</option>
                    {/if}
                    {#each workspaceProfiles as profile}
                      <option value={profile.id}>{profile.title}</option>
                    {/each}
                  </select>
                </label>

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
                    <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Working Dir</div>
                    <div class="mt-2 truncate text-sm text-zinc-100">
                      {compactPath(selectedSession.working_dir)}
                    </div>
                    <div class="mt-1 text-xs text-zinc-500">
                      {formatState(selectedSession.working_dir_kind)}
                    </div>
                  </div>
                </div>

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Routing</div>
                  <div class="mt-2 text-sm text-zinc-100">
                    {selectedSession.profile_title ||
                      selectedSession.route_title ||
                      'Direct session target'}
                  </div>
                  <div class="mt-1 text-xs text-zinc-500">
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
                  <div class="mt-2 text-sm text-zinc-100">
                    {selectedSession.provider_session_id || 'Waiting for first successful turn'}
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

              <section class="space-y-4 border-t border-zinc-900 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Projects</div>
                  <div class="text-sm text-zinc-400">
                    Attach, detach, or promote workspace projects without changing the main workspace root.
                  </div>
                </div>

                <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                  <div class="text-[11px] uppercase tracking-[0.14em] text-zinc-500">Primary Context</div>
                  <div class="mt-2 text-sm text-zinc-100">{selectedProjectTitle}</div>
                  {#if attachedProjects.length === 0}
                    <div class="mt-1 text-xs text-zinc-500">
                      This session is currently running from workspace scratch.
                    </div>
                  {/if}
                </div>

                <div class="space-y-3">
                  {#each workspaceProjects as project}
                    <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
                      <div class="flex items-start justify-between gap-3">
                        <div class="min-w-0">
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

              <section class="space-y-4 border-t border-zinc-900 pt-6">
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
                            <label class="block space-y-1">
                              <span class="text-xs text-zinc-500">{parameter.label}</span>
                              <input
                                class="h-9 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 text-sm text-zinc-100 outline-none focus:border-zinc-700"
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
                            </label>
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

              <section class="space-y-4 border-t border-zinc-900 pt-6">
                <div class="space-y-1">
                  <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Recent Activity</div>
                  <div class="text-sm text-zinc-400">
                    Audit history stays live from the daemon stream, without taking over the session page.
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
          </aside>
        {/if}
      </div>
    {/if}
  </div>
</div>
