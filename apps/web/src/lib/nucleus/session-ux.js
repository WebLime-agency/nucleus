export const RUNTIME_ACTIVE_STATES = new Set(['running', 'waiting', 'paused']);
export const REASONING_STALE_AFTER_SECONDS = 90;

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

export function childRouteLabel(record) {
  const title = String(record?.executor_route_title ?? '').trim();
  if (title) return title;

  const id = String(record?.executor_route_id ?? '').trim();
  if (id) return id;

  return '(inherits parent)';
}

export function noActivity(record, nowSeconds) {
  return reasoningActivityView(record, nowSeconds)?.stale ?? false;
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
