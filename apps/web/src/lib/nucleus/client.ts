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
  mcpServerRecordSchema,
  mcpServerSummarySchema,
  memoryEntrySchema,
  memoryEntryUpsertRequestSchema,
  memorySummarySchema,
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
  skillInstallationRecordSchema,
  skillInstallationUpsertRequestSchema,
  skillManifestSchema,
  skillImportRequestSchema,
  skillImportResponseSchema,
  skillInstallResultSchema,
  skillPackageRecordSchema,
  skillPackageUpsertRequestSchema,
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
    throw new Error('Nucleus returned an invalid response payload.');
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

export async function deleteSkill(skillId: string, fetchImpl: FetchLike = fetch) {
  await daemonFetch(fetchImpl, `/api/skills/${encodeURIComponent(skillId)}`, {
    method: 'DELETE',
    headers: { accept: 'application/json' }
  });
}

export async function importSkills(
  input: z.input<typeof skillImportRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = skillImportRequestSchema.parse(input);
  return parseJson(
    await daemonFetch(fetchImpl, '/api/skills/import', {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(payload)
    }),
    skillImportResponseSchema
  );
}

export async function reconcileSkills(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/skills/reconcile', { method: 'POST', headers: { accept: 'application/json' } }),
    skillImportResponseSchema
  );
}

export async function checkSkillUpdate(skillId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/skills/${encodeURIComponent(skillId)}/check-update`, { method: 'POST', headers: { accept: 'application/json' } }),
    skillInstallResultSchema
  );
}

export async function fetchSkillPackages(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/skill-packages', {
      headers: { accept: 'application/json' }
    }),
    z.array(skillPackageRecordSchema)
  );
}

export async function upsertSkillPackage(
  input: z.input<typeof skillPackageUpsertRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = skillPackageUpsertRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/skill-packages', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    skillPackageRecordSchema
  );
}

export async function fetchSkillInstallations(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/skill-installations', {
      headers: { accept: 'application/json' }
    }),
    z.array(skillInstallationRecordSchema)
  );
}

export async function upsertSkillInstallation(
  input: z.input<typeof skillInstallationUpsertRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = skillInstallationUpsertRequestSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/skill-installations', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    skillInstallationRecordSchema
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

export async function fetchMcpServerRecords(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/mcps', {
      headers: { accept: 'application/json' }
    }),
    z.array(mcpServerRecordSchema)
  );
}

export async function discoverMcpServer(serverId: string, fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, `/api/mcps/${encodeURIComponent(serverId)}/discover`, {
      method: 'POST',
      headers: { accept: 'application/json' }
    }),
    mcpServerSummarySchema
  );
}

export async function upsertMcpServer(
  input: z.input<typeof mcpServerRecordSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = mcpServerRecordSchema.parse(input);

  return parseJson(
    await daemonFetch(fetchImpl, '/api/mcps', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    mcpServerRecordSchema
  );
}

export async function deleteMcpServer(serverId: string, fetchImpl: FetchLike = fetch) {
  await daemonFetch(fetchImpl, `/api/mcps/${encodeURIComponent(serverId)}`, {
    method: 'DELETE',
    headers: { accept: 'application/json' }
  });
}

export async function fetchMemory(fetchImpl: FetchLike = fetch) {
  return parseJson(
    await daemonFetch(fetchImpl, '/api/memory', {
      headers: { accept: 'application/json' }
    }),
    memorySummarySchema
  );
}

export async function upsertMemory(
  input: z.input<typeof memoryEntryUpsertRequestSchema>,
  fetchImpl: FetchLike = fetch
) {
  const payload = memoryEntryUpsertRequestSchema.parse(input);
  return parseJson(
    await daemonFetch(fetchImpl, payload.id ? `/api/memory/${encodeURIComponent(payload.id)}` : '/api/memory', {
      method: payload.id ? 'PUT' : 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json'
      },
      body: JSON.stringify(payload)
    }),
    memoryEntrySchema
  );
}

export async function deleteMemory(memoryId: string, fetchImpl: FetchLike = fetch) {
  await daemonFetch(fetchImpl, `/api/memory/${encodeURIComponent(memoryId)}`, {
    method: 'DELETE',
    headers: { accept: 'application/json' }
  });
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
