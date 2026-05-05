import { z } from 'zod';

import {
  actionRunRequestSchema,
  actionRunResponseSchema,
  actionSummarySchema,
  apiErrorSchema,
  auditEventSchema,
  createSessionRequestSchema,
  processKillRequestSchema,
  processKillResponseSchema,
  processListResponseSchema,
  projectUpdateRequestSchema,
  routerProfileSummarySchema,
  runtimeOverviewSchema,
  runtimeSummarySchema,
  sessionDetailSchema,
  sessionPromptRequestSchema,
  sessionSummarySchema,
  settingsSummarySchema,
  systemStatsSchema,
  updateStatusSchema,
  updateSessionRequestSchema,
  workspaceSummarySchema,
  workspaceUpdateRequestSchema
} from './schemas';

type FetchLike = typeof fetch;
type ProcessSort = 'cpu' | 'memory';

async function parseJson<T>(response: Response, schema: z.ZodType<T>): Promise<T> {
  if (!response.ok) {
    throw new Error(await readErrorMessage(response));
  }

  const payload = await response.json().catch(() => null);
  const result = schema.safeParse(payload);

  if (!result.success) {
    throw new Error('Daemon returned an invalid response payload.');
  }

  return result.data;
}

async function readErrorMessage(response: Response): Promise<string> {
  const payload = await response.json().catch(() => null);
  const parsed = apiErrorSchema.safeParse(payload);

  if (parsed.success) {
    return parsed.data.message;
  }

  return `Request failed with ${response.status}.`;
}

export async function fetchOverview(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/overview', {
      headers: { accept: 'application/json' }
    }),
    runtimeOverviewSchema
  );
}

export async function fetchRuntimes(refresh = false, fetchImpl: FetchLike = fetch) {
  const query = refresh ? '?refresh=true' : '';

  return parseJson(
    await fetchImpl(`/api/runtimes${query}`, {
      headers: { accept: 'application/json' }
    }),
    z.array(runtimeSummarySchema)
  );
}

export async function fetchSessions(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/sessions', {
      headers: { accept: 'application/json' }
    }),
    z.array(sessionSummarySchema)
  );
}

export async function fetchWorkspace(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/workspace', {
      headers: { accept: 'application/json' }
    }),
    workspaceSummarySchema
  );
}

export async function fetchSettings(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/settings', {
      headers: { accept: 'application/json' }
    }),
    settingsSummarySchema
  );
}

export async function checkForUpdates(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/settings/update/check', {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    updateStatusSchema
  );
}

export async function applyUpdate(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/settings/update/apply', {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    updateStatusSchema
  );
}

export async function updateWorkspace(
  input: z.input<typeof workspaceUpdateRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = workspaceUpdateRequestSchema.parse(input);

  return parseJson(
    await fetchImpl('/api/workspace', {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    workspaceSummarySchema
  );
}

export async function updateProject(
  projectId: string,
  input: z.input<typeof projectUpdateRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = projectUpdateRequestSchema.parse(input);

  return parseJson(
    await fetchImpl(`/api/workspace/projects/${projectId}`, {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    workspaceSummarySchema
  );
}

export async function fetchRouterProfiles(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/router/profiles', {
      headers: { accept: 'application/json' }
    }),
    z.array(routerProfileSummarySchema)
  );
}

export async function fetchActions(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/actions', {
      headers: { accept: 'application/json' }
    }),
    z.array(actionSummarySchema)
  );
}

export async function fetchAuditEvents(limit = 20, fetchImpl: FetchLike = fetch) {
  const params = new URLSearchParams({ limit: String(limit) });

  return parseJson(
    await fetchImpl(`/api/audit?${params.toString()}`, {
      headers: { accept: 'application/json' }
    }),
    z.array(auditEventSchema)
  );
}

export async function fetchSessionDetail(sessionId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl(`/api/sessions/${sessionId}`, {
      headers: { accept: 'application/json' }
    }),
    sessionDetailSchema
  );
}

export async function createSession(
  input: z.input<typeof createSessionRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = createSessionRequestSchema.parse(input);

  return parseJson(
    await fetchImpl('/api/sessions', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    sessionDetailSchema
  );
}

export async function updateSession(
  sessionId: string,
  input: z.input<typeof updateSessionRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = updateSessionRequestSchema.parse(input);

  return parseJson(
    await fetchImpl(`/api/sessions/${sessionId}`, {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    sessionDetailSchema
  );
}

export async function deleteSession(sessionId: string, fetchImpl: FetchLike = fetch) {
  const response = await fetchImpl(`/api/sessions/${sessionId}`, {
    method: 'DELETE',
    headers: { accept: 'application/json' }
  });

  if (!response.ok && response.status !== 204) {
    throw new Error(await readErrorMessage(response));
  }
}

export async function sendSessionPrompt(
  sessionId: string,
  input: z.input<typeof sessionPromptRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = sessionPromptRequestSchema.parse(input);

  return parseJson(
    await fetchImpl(`/api/sessions/${sessionId}/prompt`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    sessionDetailSchema
  );
}

export async function runAction(
  actionId: string,
  input: z.input<typeof actionRunRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = actionRunRequestSchema.parse(input);

  return parseJson(
    await fetchImpl(`/api/actions/${actionId}/run`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    actionRunResponseSchema
  );
}

export async function fetchSystemStats(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await fetchImpl('/api/system', {
      headers: { accept: 'application/json' }
    }),
    systemStatsSchema
  );
}

export async function fetchProcesses(
  options: { sort?: ProcessSort; limit?: number } = {},
  fetchImpl: FetchLike = fetch
) {
  const params = new URLSearchParams();

  if (options.sort) {
    params.set('sort', options.sort);
  }

  if (options.limit) {
    params.set('limit', String(options.limit));
  }

  const query = params.size > 0 ? `?${params.toString()}` : '';

  return parseJson(
    await fetchImpl(`/api/system/processes${query}`, {
      headers: { accept: 'application/json' }
    }),
    processListResponseSchema
  );
}

export async function killProcess(pid: number, fetchImpl: FetchLike = fetch) {
  const payload = processKillRequestSchema.parse({ pid });

  return parseJson(
    await fetchImpl('/api/system/processes', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    processKillResponseSchema
  );
}
