import { z } from 'zod';

import { NucleusAuthError, readAccessToken } from './auth';
import {
  actionRunRequestSchema,
  actionRunResponseSchema,
  actionSummarySchema,
  apiErrorSchema,
  auditEventSchema,
  approvalResolutionRequestSchema,
  createPlaybookRequestSchema,
  createSessionRequestSchema,
  jobDetailSchema,
  jobSummarySchema,
  mcpServerSummarySchema,
  playbookDetailSchema,
  playbookSummarySchema,
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
  skillManifestSchema,
  updateConfigRequestSchema,
  systemStatsSchema,
  updateStatusSchema,
  updatePlaybookRequestSchema,
  updateSessionRequestSchema,
  workspaceProfileSummarySchema,
  workspaceProfileWriteRequestSchema,
  workspaceSummarySchema,
  workspaceUpdateRequestSchema
} from './schemas';

type FetchLike = typeof fetch;
type ProcessSort = 'cpu' | 'memory';

async function daemonFetch(fetchImpl: FetchLike, input: string, init: RequestInit = {}) {
  const headers = new Headers(init.headers ?? {});
  const token = readAccessToken();

  if (token) {
    headers.set('authorization', `Bearer ${token}`);
  }

  return fetchImpl(input, {
    ...init,
    headers
  });
}

async function parseJson<T>(response: Response, schema: z.ZodType<T>): Promise<T> {
  if (!response.ok) {
    const message = await readErrorMessage(response);

    if (response.status === 401) {
      throw new NucleusAuthError(message);
    }

    throw new Error(message);
  }

  const payload = await response.json().catch(() => null);
  const result = schema.safeParse(payload);

  if (!result.success) {
    throw new Error('Daemon returned an invalid response payload.');
  }

  return result.data;
}

async function readErrorMessage(response: Response): Promise<string> {
  if (response.status === 401) {
    return 'Authentication required. Enter a valid Nucleus access token.';
  }

  const payload = await response.json().catch(() => null);
  const parsed = apiErrorSchema.safeParse(payload);

  if (parsed.success) {
    return parsed.data.message;
  }

  return `Request failed with ${response.status}.`;
}

export async function fetchOverview(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/overview', {
      headers: { accept: 'application/json' }
    }),
    runtimeOverviewSchema
  );
}

export async function fetchRuntimes(refresh = false, fetchImpl: FetchLike = fetch) {
  const query = refresh ? '?refresh=true' : '';

  return parseJson(
    await daemonFetch(fetchImpl, `/api/runtimes${query}`, {
      headers: { accept: 'application/json' }
    }),
    z.array(runtimeSummarySchema)
  );
}

export async function fetchSessions(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/sessions', {
      headers: { accept: 'application/json' }
    }),
    z.array(sessionSummarySchema)
  );
}

export async function fetchPlaybooks(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/playbooks', {
      headers: { accept: 'application/json' }
    }),
    z.array(playbookSummarySchema)
  );
}

export async function fetchPlaybookDetail(playbookId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/playbooks/${playbookId}`, {
      headers: { accept: 'application/json' }
    }),
    playbookDetailSchema
  );
}

export async function createPlaybook(
  input: z.input<typeof createPlaybookRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = createPlaybookRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/playbooks', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    playbookDetailSchema
  );
}

export async function updatePlaybook(
  playbookId: string,
  input: z.input<typeof updatePlaybookRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = updatePlaybookRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, `/api/playbooks/${playbookId}`, {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    playbookDetailSchema
  );
}

export async function deletePlaybook(playbookId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/playbooks/${playbookId}`, {
      method: 'DELETE',
      headers: { accept: 'application/json' }
    }),
    playbookDetailSchema
  );
}

export async function runPlaybook(playbookId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/playbooks/${playbookId}/run`, {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    jobDetailSchema
  );
}

export async function fetchWorkspace(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/workspace', {
      headers: { accept: 'application/json' }
    }),
    workspaceSummarySchema
  );
}

export async function fetchSettings(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/settings', {
      headers: { accept: 'application/json' }
    }),
    settingsSummarySchema
  );
}

export async function checkForUpdates(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/settings/update/check', {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    updateStatusSchema
  );
}

export async function applyUpdate(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/settings/update/apply', {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    updateStatusSchema
  );
}

export async function restartDaemon(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/settings/restart', {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    updateStatusSchema
  );
}

export async function updateUpdateConfig(
  input: z.input<typeof updateConfigRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = updateConfigRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/settings/update-config', {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
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
    await daemonFetch(fetchImpl, '/api/workspace', {
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

export async function createWorkspaceProfile(
  input: z.input<typeof workspaceProfileWriteRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = workspaceProfileWriteRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/workspace/profiles', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    workspaceProfileSummarySchema
  );
}

export async function updateWorkspaceProfile(
  profileId: string,
  input: z.input<typeof workspaceProfileWriteRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = workspaceProfileWriteRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, `/api/workspace/profiles/${profileId}`, {
      method: 'PATCH',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    workspaceProfileSummarySchema
  );
}

export async function deleteWorkspaceProfile(profileId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/workspace/profiles/${profileId}`, {
      method: 'DELETE',
      headers: { accept: 'application/json' }
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
    await daemonFetch(fetchImpl, `/api/workspace/projects/${projectId}`, {
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
    await daemonFetch(fetchImpl, '/api/router/profiles', {
      headers: { accept: 'application/json' }
    }),
    z.array(routerProfileSummarySchema)
  );
}

export async function fetchSkills(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/skills', {
      headers: { accept: 'application/json' }
    }),
    z.array(skillManifestSchema)
  );
}

export async function upsertSkill(
  input: z.input<typeof skillManifestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = skillManifestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/skills', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    skillManifestSchema
  );
}

export async function fetchMcpServers(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/mcps', {
      headers: { accept: 'application/json' }
    }),
    z.array(mcpServerSummarySchema)
  );
}

export async function upsertMcpServer(
  input: z.input<typeof mcpServerSummarySchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = mcpServerSummarySchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/mcps', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    mcpServerSummarySchema
  );
}

export async function fetchActions(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/actions', {
      headers: { accept: 'application/json' }
    }),
    z.array(actionSummarySchema)
  );
}

export async function fetchAuditEvents(limit = 20, fetchImpl: FetchLike = fetch) {
  const params = new URLSearchParams({ limit: String(limit) });

  return parseJson(
    await daemonFetch(fetchImpl, `/api/audit?${params.toString()}`, {
      headers: { accept: 'application/json' }
    }),
    z.array(auditEventSchema)
  );
}

export async function fetchSessionDetail(sessionId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/sessions/${sessionId}`, {
      headers: { accept: 'application/json' }
    }),
    sessionDetailSchema
  );
}

export async function fetchSessionJobs(sessionId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/sessions/${sessionId}/jobs`, {
      headers: { accept: 'application/json' }
    }),
    z.array(jobSummarySchema)
  );
}

export async function fetchJobDetail(jobId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/jobs/${jobId}`, {
      headers: { accept: 'application/json' }
    }),
    jobDetailSchema
  );
}

export async function cancelJob(jobId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/jobs/${jobId}/cancel`, {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    jobDetailSchema
  );
}

export async function resumeJob(jobId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/jobs/${jobId}/resume`, {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    jobDetailSchema
  );
}

export async function approveRequest(
  approvalId: string,
  input: z.input<typeof approvalResolutionRequestSchema> = {},
  fetchImpl: FetchLike = fetch
) {
  const payload = approvalResolutionRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, `/api/approvals/${approvalId}/approve`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    jobDetailSchema
  );
}

export async function denyRequest(
  approvalId: string,
  input: z.input<typeof approvalResolutionRequestSchema> = {},
  fetchImpl: FetchLike = fetch
) {
  const payload = approvalResolutionRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, `/api/approvals/${approvalId}/deny`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    jobDetailSchema
  );
}

export async function createSession(
  input: z.input<typeof createSessionRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = createSessionRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/sessions', {
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
    await daemonFetch(fetchImpl, `/api/sessions/${sessionId}`, {
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
  const response = await daemonFetch(fetchImpl, `/api/sessions/${sessionId}`, {
    method: 'DELETE',
    headers: { accept: 'application/json' }
  });

  if (!response.ok && response.status !== 204) {
    const message = await readErrorMessage(response);

    if (response.status === 401) {
      throw new NucleusAuthError(message);
    }

    throw new Error(message);
  }
}

export async function sendSessionPrompt(
  sessionId: string,
  input: z.input<typeof sessionPromptRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = sessionPromptRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, `/api/sessions/${sessionId}/prompt`, {
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
    await daemonFetch(fetchImpl, `/api/actions/${actionId}/run`, {
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
    await daemonFetch(fetchImpl, '/api/system', {
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
    await daemonFetch(fetchImpl, `/api/system/processes${query}`, {
      headers: { accept: 'application/json' }
    }),
    processListResponseSchema
  );
}

export async function killProcess(pid: number, fetchImpl: FetchLike = fetch) {
  const payload = processKillRequestSchema.parse({ pid });

  return parseJson(
    await daemonFetch(fetchImpl, '/api/system/processes', {
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
