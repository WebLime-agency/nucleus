export const RUNTIME_ACTIVE_STATES = new Set(['running', 'waiting', 'paused']);
export const REASONING_STALE_AFTER_SECONDS = 90;
export const ACTIVITY_STALE_AFTER_SECONDS = 120;

export function runtimeStartSeconds(record) {
  const createdAt = Number(record?.created_at ?? 0);
  const lastResumedAt = Number(record?.last_resumed_at ?? 0);
  return Math.max(createdAt, lastResumedAt);
}

export function runtimeBadgeView(record, nowSeconds) {
  if (!record || !RUNTIME_ACTIVE_STATES.has(record.state)) {
    return null;
  }

  const start = runtimeStartSeconds(record);
  if (!start) {
    return null;
  }

  const elapsedSeconds = Math.max(0, nowSeconds - start);
  return {
    label: formatDuration(elapsedSeconds),
    tone: runtimeTone(elapsedSeconds),
    title: `Started ${formatDateTime(start)}`,
    elapsedSeconds,
    start
  };
}

export function runtimeTone(elapsedSeconds) {
  if (elapsedSeconds < 5 * 60) return 'green';
  if (elapsedSeconds < 60 * 60) return 'yellow';
  if (elapsedSeconds < 4 * 60 * 60) return 'orange';
  return 'red';
}

export function nextRuntimeTick(nowSeconds) {
  return nowSeconds + 10;
}

export function reasoningActivityView(record, nowSeconds) {
  const lastReasoning = String(record?.last_reasoning ?? '').trim();
  const lastReasoningAt = Number(record?.last_reasoning_at ?? 0);
  if (!lastReasoning || !lastReasoningAt) {
    return null;
  }

  const ageSeconds = Math.max(0, nowSeconds - lastReasoningAt);
  const stale = RUNTIME_ACTIVE_STATES.has(record.state) && ageSeconds > REASONING_STALE_AFTER_SECONDS;
  return {
    text: `last reasoning · ${formatDuration(ageSeconds)} ago: ${lastReasoning}`,
    stale,
    ageSeconds
  };
}

export function activityFreshnessView(record, nowSeconds) {
  if (!record || !RUNTIME_ACTIVE_STATES.has(record.state)) {
    return null;
  }

  const lastReasoning = String(record.last_reasoning ?? '').trim();
  const lastReasoningAt = Number(record.last_reasoning_at ?? 0);
  const updatedAt = Number(record.updated_at ?? 0);
  const createdAt = Number(record.created_at ?? 0);
  const lastActivityAt = Math.max(lastReasoningAt, updatedAt, createdAt);

  if (!lastActivityAt) {
    return null;
  }

  const ageSeconds = Math.max(0, nowSeconds - lastActivityAt);
  const stale = ageSeconds > ACTIVITY_STALE_AFTER_SECONDS;
  const stalePrefix = stale ? 'No worker updates' : 'Last worker update';

  return {
    text: lastReasoning
      ? `${stalePrefix} for ${formatDuration(ageSeconds)}; latest reasoning: ${lastReasoning}`
      : `${stalePrefix} for ${formatDuration(ageSeconds)}.`,
    stale,
    ageSeconds,
    lastActivityAt
  };
}

export function jobDetailActivityFreshnessView(detail, nowSeconds) {
  if (!detail?.job) {
    return null;
  }

  const records = [
    detail.job,
    ...(Array.isArray(detail.workers) ? detail.workers : []),
    ...(Array.isArray(detail.child_jobs) ? detail.child_jobs : [])
  ];
  const latestReasoningRecord = records.reduce((latest, record) => {
    if (!String(record?.last_reasoning ?? '').trim()) return latest;
    if (!latest) return record;
    return Number(record.last_reasoning_at ?? 0) >= Number(latest.last_reasoning_at ?? 0)
      ? record
      : latest;
  }, null);
  const lastActivityAt = Math.max(
    freshnessTimestamp(detail.job),
    ...records.map(freshnessTimestamp),
    ...(Array.isArray(detail.tool_calls) ? detail.tool_calls : []).map(freshnessTimestamp),
    ...(Array.isArray(detail.command_sessions) ? detail.command_sessions : []).map(freshnessTimestamp),
    ...(Array.isArray(detail.approvals) ? detail.approvals : []).map(freshnessTimestamp),
    ...(Array.isArray(detail.artifacts) ? detail.artifacts : []).map(freshnessTimestamp),
    ...(Array.isArray(detail.events) ? detail.events : []).map(freshnessTimestamp)
  );

  return activityFreshnessView(
    {
      ...detail.job,
      updated_at: Math.max(Number(detail.job.updated_at ?? 0), lastActivityAt),
      last_reasoning: latestReasoningRecord?.last_reasoning ?? detail.job.last_reasoning,
      last_reasoning_at: latestReasoningRecord?.last_reasoning_at ?? detail.job.last_reasoning_at
    },
    nowSeconds
  );
}

export function reasoningActivityDisplayView(record, nowSeconds) {
  return activityFreshnessView(record, nowSeconds) ?? reasoningActivityView(record, nowSeconds);
}

export function shouldShowActivityStaleWarning(freshness, detail, pendingApproval) {
  if (!freshness?.stale) {
    return false;
  }

  if (pendingApproval?.state === 'pending') {
    return false;
  }

  return !isPausedApprovalWait(detail);
}

export function usageView(record) {
  if (!record?.token_usage_known) {
    return {
      label: 'tokens unknown',
      title: 'Provider did not return token usage for this worker yet.',
      hasPrice: false
    };
  }

  const promptTokens = Number(record.prompt_tokens ?? 0);
  const completionTokens = Number(record.completion_tokens ?? 0);
  const cachedTokens = Number(record.cached_tokens ?? 0);
  const totalTokens = promptTokens + completionTokens;
  const tokenLabel = `${formatCount(totalTokens)} tokens`;
  const cost = typeof record.cost_usd_estimate === 'number' ? record.cost_usd_estimate : null;

  return {
    label: cost === null ? `${tokenLabel} · $— (no price)` : `${tokenLabel} · ${formatUsd(cost)}`,
    title: [
      `Prompt: ${formatCount(promptTokens)}`,
      `Completion: ${formatCount(completionTokens)}`,
      `Cached: ${formatCount(cachedTokens)}`,
      cost === null ? 'Price: unavailable for this model' : `Estimate: ${formatUsd(cost)}`
    ].join(' · '),
    hasPrice: cost !== null,
    totalTokens
  };
}

export function completionGateGroups(record) {
  const gates = Array.isArray(record?.completion_gates) ? record.completion_gates : [];
  return {
    blocked: gates.filter((gate) => gate.state === 'blocked'),
    pending: gates.filter((gate) => gate.state === 'pending'),
    done: gates.filter((gate) => gate.state === 'done')
  };
}

export function gateBadgeVariant(state) {
  if (state === 'done') return 'default';
  if (state === 'blocked') return 'destructive';
  return 'warning';
}

export function publicationOutcomeBadgeVariant(status) {
  if (status === 'opened' || status === 'merged') return 'default';
  if (status === 'failed' || status === 'blocked' || status === 'not_opened') {
    return 'destructive';
  }
  if (status === 'not_requested') return 'secondary';
  return 'warning';
}

export function publicationOutcomeLabel(job) {
  if (!job?.publication_requested) return '';

  if (job.publication_status === 'opened') {
    return job.browser_verification_status === 'passed'
      ? 'PR opened, browser-verified'
      : 'PR opened, not browser-verified';
  }

  if (job.publication_status === 'merged') {
    return job.browser_verification_status === 'passed'
      ? 'PR merged, browser-verified'
      : 'PR merged, not browser-verified';
  }

  if (job.publication_status === 'blocked') {
    if (job.validation_status === 'failed') return 'Blocked, validation failed';
    if (
      job.browser_verification_status === 'unavailable' ||
      job.browser_verification_status === 'not_performed'
    ) {
      return 'Blocked, not browser-verified';
    }
    return 'PR publication blocked';
  }

  if (job.publication_status === 'failed') return 'PR publication failed';
  if (job.publication_status === 'not_opened') return 'PR not opened';
  return 'Publication requested';
}

export function childRouteLabel(record) {
  const title = String(record?.executor_route_title ?? '').trim();
  if (title) return title;

  const id = String(record?.executor_route_id ?? '').trim();
  if (id) return id;

  return '(inherits parent)';
}

export function noActivity(record, nowSeconds) {
  return activityFreshnessView(record, nowSeconds)?.stale ?? false;
}

export function activityFailureView(detail, activeProgress) {
  if (!detail) return null;
  if (SUPPRESSED_FAILURE_JOB_STATES.has(detail.job?.state)) return null;

  const progressTime = activityTimestamp(activeProgress);
  const failedCommand = latestByTimestamp(
    (Array.isArray(detail.command_sessions) ? detail.command_sessions : []).filter(
      (item) =>
        FAILURE_STATES.has(item?.state) &&
        String(item?.last_error ?? '').trim() &&
        failureIsCurrent(detail, item.last_error, activityTimestamp(item), progressTime)
    )
  );
  const failedTool = latestByTimestamp(
    (Array.isArray(detail.tool_calls) ? detail.tool_calls : []).filter(
      (item) =>
        item?.status === 'failed' &&
        String(item?.error_detail ?? '').trim() &&
        failureIsCurrent(detail, item.error_detail, activityTimestamp(item), progressTime)
    )
  );
  const commandTime = activityTimestamp(failedCommand);
  const toolTime = activityTimestamp(failedTool);

  if (failedCommand && commandTime >= toolTime) {
    return {
      title: commandSessionTitle(failedCommand),
      detail: String(failedCommand.last_error).trim(),
      state: failedCommand.state
    };
  }

  if (failedTool) {
    return {
      title: `${toolLabel(failedTool.tool_id)} failed`,
      detail: String(failedTool.error_detail).trim(),
      state: failedTool.status
    };
  }

  const jobError = String(detail.job?.last_error ?? '').trim();
  if (jobError && shouldShowJobError(detail.job, progressTime)) {
    return {
      title: detail.job?.title || 'Utility Worker issue',
      detail: jobError,
      state: detail.job?.state || 'failed'
    };
  }

  return null;
}

const FAILURE_STATES = new Set(['failed', 'timed_out', 'error']);
const SUPPRESSED_FAILURE_JOB_STATES = new Set([
  'approved',
  'completed',
  'canceled',
  'closed',
  'denied',
  'orphaned'
]);

function shouldShowJobError(job, progressTime) {
  if (!progressTime) return true;
  if (FAILURE_STATES.has(job?.state)) return true;
  return activityTimestamp(job) >= progressTime;
}

function failureIsCurrent(detail, errorText, failureTime, progressTime) {
  if (!progressTime || failureTime >= progressTime) {
    return true;
  }

  const failure = String(errorText ?? '').trim();
  if (!failure) return false;

  return currentErrorTexts(detail).some((current) => current.includes(failure) || failure.includes(current));
}

function currentErrorTexts(detail) {
  return [
    detail?.job?.last_error,
    ...(Array.isArray(detail?.workers) ? detail.workers.map((worker) => worker?.last_error) : [])
  ]
    .map((value) => String(value ?? '').trim())
    .filter(Boolean);
}

function latestByTimestamp(items) {
  return items.reduce((latest, item) => {
    if (!latest) return item;
    return activityTimestamp(item) >= activityTimestamp(latest) ? item : latest;
  }, null);
}

function activityTimestamp(item) {
  return Number(item?.completed_at ?? item?.updated_at ?? item?.created_at ?? 0);
}

function freshnessTimestamp(item) {
  return Math.max(
    Number(item?.last_reasoning_at ?? 0),
    Number(item?.completed_at ?? 0),
    Number(item?.updated_at ?? 0),
    Number(item?.started_at ?? 0),
    Number(item?.resolved_at ?? 0),
    Number(item?.requested_at ?? 0),
    Number(item?.created_at ?? 0)
  );
}

function isPausedApprovalWait(detail) {
  if (!detail?.job || detail.job.state !== 'paused') {
    return false;
  }

  if (Number(detail.job.pending_approval_count ?? 0) > 0) {
    return true;
  }

  return (Array.isArray(detail.approvals) ? detail.approvals : []).some(
    (approval) => approval?.state === 'pending'
  );
}

function commandSessionTitle(commandSession) {
  const title = String(commandSession?.title ?? '').trim();
  if (title) return title;

  const command = String(commandSession?.command ?? '').trim();
  return command ? `Command ${command}` : 'Command failed';
}

function toolLabel(toolId) {
  if (toolId === 'python.run') return 'Python runtime';
  if (toolId === 'command.run') return 'Command';
  return String(toolId ?? 'Tool').replaceAll('_', ' ');
}

function formatDuration(seconds) {
  const total = Math.max(0, Math.round(seconds));
  if (total >= 3600) return `${Math.floor(total / 3600)}h ${Math.floor((total % 3600) / 60)}m`;
  if (total >= 60) return `${Math.floor(total / 60)}m ${total % 60}s`;
  return `${total}s`;
}

function formatDateTime(timestampSeconds) {
  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit'
  }).format(timestampSeconds * 1000);
}

function formatCount(value) {
  return new Intl.NumberFormat().format(Math.max(0, value));
}

function formatUsd(value) {
  if (value < 0.01 && value > 0) {
    return `$${value.toFixed(4)}`;
  }

  return `$${value.toFixed(2)}`;
}
