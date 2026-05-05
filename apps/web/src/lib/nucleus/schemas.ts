import { z } from 'zod';

export const healthResponseSchema = z.object({
  status: z.string(),
  service: z.string(),
  version: z.string()
});

export const runtimeSummarySchema = z.object({
  id: z.string(),
  summary: z.string(),
  state: z.string(),
  auth_state: z.string(),
  version: z.string(),
  executable_path: z.string(),
  default_model: z.string(),
  note: z.string(),
  supports_sessions: z.boolean(),
  supports_prompting: z.boolean()
});

export const sessionProjectSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  slug: z.string(),
  relative_path: z.string(),
  absolute_path: z.string(),
  is_primary: z.boolean()
});

export const sessionSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  route_id: z.string(),
  route_title: z.string(),
  project_id: z.string(),
  project_title: z.string(),
  project_path: z.string(),
  provider: z.string(),
  model: z.string(),
  working_dir: z.string(),
  working_dir_kind: z.string(),
  scope: z.string(),
  project_count: z.number().int().nonnegative(),
  projects: z.array(sessionProjectSummarySchema),
  state: z.string(),
  provider_session_id: z.string(),
  last_error: z.string(),
  last_message_excerpt: z.string(),
  turn_count: z.number().int().nonnegative(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const sessionTurnImageSchema = z.object({
  display_name: z.string(),
  mime_type: z.string(),
  data_url: z.string()
});

export const sessionTurnSchema = z.object({
  id: z.string(),
  session_id: z.string(),
  role: z.string(),
  content: z.string(),
  images: z.array(sessionTurnImageSchema).default([]),
  created_at: z.number().int()
});

export const sessionDetailSchema = z.object({
  session: sessionSummarySchema,
  turns: z.array(sessionTurnSchema)
});

export const promptProgressUpdateSchema = z.object({
  session_id: z.string(),
  status: z.string(),
  label: z.string(),
  detail: z.string(),
  provider: z.string(),
  model: z.string(),
  route_id: z.string(),
  route_title: z.string(),
  attempt: z.number().int().nonnegative(),
  attempt_count: z.number().int().nonnegative(),
  created_at: z.number().int()
});

export const createSessionRequestSchema = z.object({
  route_id: z.string().trim().optional(),
  provider: z.string().trim().optional(),
  title: z.string().trim().optional(),
  model: z.string().trim().optional(),
  project_id: z.string().trim().optional(),
  primary_project_id: z.string().trim().optional(),
  project_ids: z.array(z.string().trim()).optional()
});

export const updateSessionRequestSchema = z.object({
  title: z.string().trim().optional(),
  route_id: z.string().trim().optional(),
  provider: z.string().trim().optional(),
  model: z.string().trim().optional(),
  state: z.string().trim().optional(),
  project_id: z.string().trim().optional(),
  primary_project_id: z.string().trim().optional(),
  project_ids: z.array(z.string().trim()).optional()
});

export const sessionPromptRequestSchema = z.object({
  prompt: z.string().default(''),
  images: z.array(sessionTurnImageSchema).default([])
}).refine((value) => value.prompt.trim().length > 0 || value.images.length > 0, {
  message: 'A prompt or at least one image is required.'
});

export const actionParameterSchema = z.object({
  name: z.string(),
  label: z.string(),
  value_type: z.string(),
  required: z.boolean(),
  description: z.string(),
  default_value: z.string()
});

export const actionSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  category: z.string(),
  summary: z.string(),
  risk: z.string(),
  requires_confirmation: z.boolean(),
  parameters: z.array(actionParameterSchema)
});

export const actionRunRequestSchema = z.object({
  params: z.record(z.string(), z.unknown()).default({})
});

export const actionRunResponseSchema = z.object({
  action_id: z.string(),
  status: z.string(),
  message: z.string(),
  result: z.unknown(),
  audit_event_id: z.number().int().optional()
});

export const projectSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  slug: z.string(),
  relative_path: z.string(),
  absolute_path: z.string(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const workspaceSummarySchema = z.object({
  root_path: z.string(),
  main_target: z.string(),
  utility_target: z.string(),
  projects: z.array(projectSummarySchema)
});

export const workspaceUpdateRequestSchema = z.object({
  root_path: z.string().trim().min(1).optional(),
  main_target: z.string().trim().min(1).optional(),
  utility_target: z.string().trim().min(1).optional()
});

export const projectUpdateRequestSchema = z.object({
  title: z.string().trim().optional()
});

export const routeTargetSchema = z.object({
  provider: z.string(),
  model: z.string()
});

export const routerProfileSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  summary: z.string(),
  enabled: z.boolean(),
  state: z.string(),
  targets: z.array(routeTargetSchema)
});

export const auditEventSchema = z.object({
  id: z.number().int().nonnegative(),
  kind: z.string(),
  target: z.string(),
  status: z.string(),
  summary: z.string(),
  detail: z.string(),
  created_at: z.number().int()
});

export const hostStatusSchema = z.object({
  hostname: z.string(),
  cpu_usage_percent: z.number(),
  memory_used_bytes: z.number(),
  memory_total_bytes: z.number(),
  process_count: z.number().int().nonnegative()
});

export const storageSummarySchema = z.object({
  state_dir: z.string(),
  database_path: z.string(),
  artifacts_dir: z.string(),
  memory_dir: z.string(),
  transcripts_dir: z.string(),
  playbooks_dir: z.string(),
  scratch_dir: z.string()
});

export const instanceSummarySchema = z.object({
  name: z.string(),
  repo_root: z.string(),
  daemon_bind: z.string(),
  install_mode: z.string()
});

export const authSummarySchema = z.object({
  enabled: z.boolean(),
  token_path: z.string()
});

export const connectionSummarySchema = z.object({
  local_url: z.string(),
  hostname_url: z.string().nullable(),
  tailscale_url: z.string().nullable(),
  web_mode: z.string(),
  web_root: z.string().nullable()
});

export const updateStatusSchema = z.object({
  install_mode: z.string(),
  repo_root: z.string(),
  branch: z.string(),
  remote_name: z.string(),
  remote_url: z.string(),
  current_commit: z.string(),
  current_commit_short: z.string(),
  remote_commit: z.string(),
  remote_commit_short: z.string(),
  update_available: z.boolean(),
  dirty_worktree: z.boolean(),
  restart_required: z.boolean(),
  checked_at: z.number().int().nullable(),
  state: z.string(),
  message: z.string()
});

export const settingsSummarySchema = z.object({
  product: z.string(),
  version: z.string(),
  instance: instanceSummarySchema,
  storage: storageSummarySchema,
  auth: authSummarySchema,
  connection: connectionSummarySchema,
  update: updateStatusSchema
});

export const runtimeOverviewSchema = z.object({
  product: z.string(),
  version: z.string(),
  runtimes: z.array(runtimeSummarySchema),
  router_profiles: z.array(routerProfileSummarySchema),
  workspace: workspaceSummarySchema,
  sessions: z.array(sessionSummarySchema),
  host: hostStatusSchema,
  storage: storageSummarySchema
});

export const cpuCoreStatSchema = z.object({
  id: z.number().int().nonnegative(),
  usage_percent: z.number(),
  frequency_mhz: z.number().int().nonnegative()
});

export const cpuStatsSchema = z.object({
  load_percent: z.number(),
  cores: z.array(cpuCoreStatSchema)
});

export const memoryStatsSchema = z.object({
  total_bytes: z.number().int().nonnegative(),
  used_bytes: z.number().int().nonnegative(),
  free_bytes: z.number().int().nonnegative(),
  available_bytes: z.number().int().nonnegative(),
  used_percent: z.number()
});

export const diskStatSchema = z.object({
  name: z.string(),
  mount_point: z.string(),
  file_system: z.string(),
  total_bytes: z.number().int().nonnegative(),
  used_bytes: z.number().int().nonnegative(),
  available_bytes: z.number().int().nonnegative()
});

export const systemStatsSchema = z.object({
  hostname: z.string(),
  current_user: z.string(),
  process_count: z.number().int().nonnegative(),
  cpu: cpuStatsSchema,
  memory: memoryStatsSchema,
  disks: z.array(diskStatSchema)
});

export const processSnapshotSchema = z.object({
  pid: z.number().int().positive(),
  name: z.string(),
  command: z.string(),
  params: z.string(),
  user: z.string(),
  cwd: z.string(),
  status: z.string(),
  cpu_percent: z.number(),
  memory_bytes: z.number().int().nonnegative(),
  memory_percent: z.number()
});

export const processListResponseSchema = z.object({
  processes: z.array(processSnapshotSchema),
  meta: z.object({
    total_processes: z.number().int().nonnegative(),
    matching_processes: z.number().int().nonnegative(),
    current_user: z.string(),
    sort: z.enum(['cpu', 'memory'])
  })
});

export const processKillRequestSchema = z.object({
  pid: z.number().int().positive()
});

export const processKillResponseSchema = z.object({
  killed_pid: z.number().int().positive(),
  name: z.string(),
  signal: z.string()
});

export const streamConnectedSchema = z.object({
  service: z.string(),
  version: z.string()
});

export const processStreamUpdateSchema = z.object({
  sort: z.enum(['cpu', 'memory']),
  response: processListResponseSchema
});

export const daemonEventSchema = z.discriminatedUnion('event', [
  z.object({
    event: z.literal('connected'),
    data: streamConnectedSchema
  }),
  z.object({
    event: z.literal('overview.updated'),
    data: runtimeOverviewSchema
  }),
  z.object({
    event: z.literal('session.updated'),
    data: sessionDetailSchema
  }),
  z.object({
    event: z.literal('prompt.progress'),
    data: promptProgressUpdateSchema
  }),
  z.object({
    event: z.literal('audit.updated'),
    data: z.array(auditEventSchema)
  }),
  z.object({
    event: z.literal('system.updated'),
    data: systemStatsSchema
  }),
  z.object({
    event: z.literal('processes.updated'),
    data: processStreamUpdateSchema
  }),
  z.object({
    event: z.literal('update.updated'),
    data: updateStatusSchema
  })
]);

export const apiErrorSchema = z.object({
  error: z.string(),
  message: z.string()
});

export type RuntimeOverview = z.infer<typeof runtimeOverviewSchema>;
export type RuntimeSummary = z.infer<typeof runtimeSummarySchema>;
export type SessionSummary = z.infer<typeof sessionSummarySchema>;
export type SessionProjectSummary = z.infer<typeof sessionProjectSummarySchema>;
export type SessionTurnImage = z.infer<typeof sessionTurnImageSchema>;
export type SessionTurn = z.infer<typeof sessionTurnSchema>;
export type SessionDetail = z.infer<typeof sessionDetailSchema>;
export type PromptProgressUpdate = z.infer<typeof promptProgressUpdateSchema>;
export type ActionSummary = z.infer<typeof actionSummarySchema>;
export type ActionRunResponse = z.infer<typeof actionRunResponseSchema>;
export type AuditEvent = z.infer<typeof auditEventSchema>;
export type ProjectSummary = z.infer<typeof projectSummarySchema>;
export type WorkspaceSummary = z.infer<typeof workspaceSummarySchema>;
export type RouterProfileSummary = z.infer<typeof routerProfileSummarySchema>;
export type RouteTarget = z.infer<typeof routeTargetSchema>;
export type SystemStats = z.infer<typeof systemStatsSchema>;
export type ProcessListResponse = z.infer<typeof processListResponseSchema>;
export type ProcessSnapshot = z.infer<typeof processSnapshotSchema>;
export type DiskStat = z.infer<typeof diskStatSchema>;
export type SettingsSummary = z.infer<typeof settingsSummarySchema>;
export type UpdateStatus = z.infer<typeof updateStatusSchema>;
export type DaemonEvent = z.infer<typeof daemonEventSchema>;
