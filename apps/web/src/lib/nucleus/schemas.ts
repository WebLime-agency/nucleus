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

export const runBudgetSummarySchema = z.object({
  mode: z.string().default('standard'),
  max_steps: z.number().int().nonnegative(),
  max_tool_calls: z.number().int().nonnegative(),
  max_wall_clock_secs: z.number().int().nonnegative()
});

export const sessionSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  profile_id: z.string(),
  profile_title: z.string(),
  route_id: z.string(),
  route_title: z.string(),
  project_id: z.string(),
  project_title: z.string(),
  project_path: z.string(),
  provider: z.string(),
  model: z.string(),
  provider_base_url: z.string(),
  provider_api_key: z.string(),
  working_dir: z.string(),
  working_dir_kind: z.string(),
  workspace_mode: z.string().default('shared_project_root'),
  source_project_path: z.string().default(''),
  git_root: z.string().default(''),
  worktree_path: z.string().default(''),
  git_branch: z.string().default(''),
  git_base_ref: z.string().default(''),
  git_head: z.string().default(''),
  git_dirty: z.boolean().default(false),
  git_untracked_count: z.number().int().nonnegative().default(0),
  git_remote_tracking_branch: z.string().default(''),
  workspace_warnings: z.array(z.string()).default([]),
  approval_mode: z.string().default('ask'),
  execution_mode: z.string().default('act'),
  run_budget_mode: z.string().default('inherit'),
  run_budget: runBudgetSummarySchema.default({
    mode: 'standard',
    max_steps: 80,
    max_tool_calls: 160,
    max_wall_clock_secs: 7200
  }),
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

export const policyDecisionSummarySchema = z.object({
  decision: z.string(),
  reason: z.string(),
  matched_rule: z.string(),
  scope_kind: z.string(),
  risk_level: z.string()
});

export const toolCapabilitySummarySchema = z.object({
  tool_id: z.string(),
  summary: z.string(),
  approval_mode: z.string(),
  risk_level: z.string(),
  side_effect_level: z.string(),
  timeout_secs: z.number().int().nonnegative(),
  max_output_bytes: z.number().int().nonnegative(),
  supports_streaming: z.boolean(),
  concurrency_group: z.string(),
  scope_kind: z.string()
});

export const jobSummarySchema = z.object({
  id: z.string(),
  session_id: z.string().nullable(),
  parent_job_id: z.string().nullable(),
  template_id: z.string().nullable(),
  title: z.string(),
  purpose: z.string(),
  trigger_kind: z.string(),
  state: z.string(),
  requested_by: z.string(),
  prompt_excerpt: z.string(),
  root_worker_id: z.string().nullable(),
  visible_turn_id: z.string().nullable(),
  result_summary: z.string(),
  last_error: z.string(),
  worker_count: z.number().int().nonnegative(),
  pending_approval_count: z.number().int().nonnegative(),
  artifact_count: z.number().int().nonnegative(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const workerSummarySchema = z.object({
  id: z.string(),
  job_id: z.string(),
  parent_worker_id: z.string().nullable(),
  title: z.string(),
  lane: z.string(),
  state: z.string(),
  provider: z.string(),
  model: z.string(),
  provider_base_url: z.string(),
  provider_api_key: z.string(),
  provider_session_id: z.string(),
  working_dir: z.string(),
  read_roots: z.array(z.string()).default([]),
  write_roots: z.array(z.string()).default([]),
  max_steps: z.number().int().nonnegative(),
  max_tool_calls: z.number().int().nonnegative(),
  max_wall_clock_secs: z.number().int().nonnegative(),
  step_count: z.number().int().nonnegative(),
  tool_call_count: z.number().int().nonnegative(),
  last_error: z.string(),
  capabilities: z.array(toolCapabilitySummarySchema).default([]),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const toolCallSummarySchema = z.object({
  id: z.string(),
  job_id: z.string(),
  worker_id: z.string(),
  tool_id: z.string(),
  status: z.string(),
  summary: z.string(),
  args_json: z.unknown(),
  result_json: z.unknown().nullable(),
  policy_decision: policyDecisionSummarySchema.nullable(),
  artifact_ids: z.array(z.string()).default([]),
  error_class: z.string(),
  error_detail: z.string(),
  created_at: z.number().int(),
  started_at: z.number().int().nullable(),
  completed_at: z.number().int().nullable()
});

export const approvalRequestSummarySchema = z.object({
  id: z.string(),
  job_id: z.string(),
  worker_id: z.string(),
  tool_call_id: z.string(),
  state: z.string(),
  risk_level: z.string(),
  summary: z.string(),
  detail: z.string(),
  diff_preview: z.string(),
  policy_decision: policyDecisionSummarySchema,
  resolution_note: z.string(),
  resolved_by: z.string(),
  requested_at: z.number().int(),
  resolved_at: z.number().int().nullable()
});

export const artifactSummarySchema = z.object({
  id: z.string(),
  job_id: z.string(),
  worker_id: z.string().nullable(),
  tool_call_id: z.string().nullable(),
  command_session_id: z.string().nullable(),
  kind: z.string(),
  title: z.string(),
  path: z.string(),
  mime_type: z.string(),
  size_bytes: z.number().int().nonnegative(),
  preview_text: z.string(),
  created_at: z.number().int()
});

export const commandSessionSummarySchema = z.object({
  id: z.string(),
  job_id: z.string(),
  worker_id: z.string(),
  tool_call_id: z.string().nullable(),
  mode: z.string(),
  title: z.string(),
  state: z.string(),
  command: z.string(),
  args: z.array(z.string()).default([]),
  cwd: z.string(),
  session_id: z.string().default(''),
  project_id: z.string().default(''),
  worktree_path: z.string().default(''),
  branch: z.string().default(''),
  port: z.number().int().nonnegative().nullable().default(null),
  network_policy: z.string(),
  timeout_secs: z.number().int().nonnegative(),
  output_limit_bytes: z.number().int().nonnegative(),
  last_error: z.string(),
  exit_code: z.number().int().nullable(),
  stdout_artifact_id: z.string().nullable(),
  stderr_artifact_id: z.string().nullable(),
  started_at: z.number().int().nullable(),
  completed_at: z.number().int().nullable(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const jobEventSchema = z.object({
  id: z.number().int().nonnegative(),
  job_id: z.string(),
  worker_id: z.string().nullable(),
  event_type: z.string(),
  status: z.string(),
  summary: z.string(),
  detail: z.string(),
  data_json: z.unknown(),
  created_at: z.number().int()
});

export const jobDetailSchema = z.object({
  job: jobSummarySchema,
  workers: z.array(workerSummarySchema).default([]),
  child_jobs: z.array(jobSummarySchema).default([]),
  tool_calls: z.array(toolCallSummarySchema).default([]),
  approvals: z.array(approvalRequestSummarySchema).default([]),
  artifacts: z.array(artifactSummarySchema).default([]),
  command_sessions: z.array(commandSessionSummarySchema).default([]),
  events: z.array(jobEventSchema).default([])
});

export const playbookSummarySchema = z.object({
  id: z.string(),
  session_id: z.string(),
  title: z.string(),
  description: z.string(),
  prompt_excerpt: z.string(),
  enabled: z.boolean(),
  policy_bundle: z.string(),
  trigger_kind: z.string(),
  schedule_interval_secs: z.number().int().nonnegative().nullable(),
  event_kind: z.string().nullable(),
  profile_id: z.string(),
  profile_title: z.string(),
  project_id: z.string(),
  project_title: z.string(),
  working_dir: z.string(),
  job_count: z.number().int().nonnegative(),
  last_job_id: z.string().nullable(),
  last_job_state: z.string(),
  last_run_at: z.number().int().nullable(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const playbookDetailSchema = z.object({
  playbook: playbookSummarySchema,
  session: sessionSummarySchema,
  prompt: z.string(),
  recent_jobs: z.array(jobSummarySchema).default([])
});

export const promptProgressUpdateSchema = z.object({
  session_id: z.string(),
  status: z.string(),
  label: z.string(),
  detail: z.string(),
  provider: z.string(),
  model: z.string(),
  profile_id: z.string(),
  profile_title: z.string(),
  route_id: z.string(),
  route_title: z.string(),
  attempt: z.number().int().nonnegative(),
  attempt_count: z.number().int().nonnegative(),
  created_at: z.number().int()
});

export const createSessionRequestSchema = z.object({
  profile_id: z.string().trim().optional(),
  route_id: z.string().trim().optional(),
  provider: z.string().trim().optional(),
  title: z.string().trim().optional(),
  model: z.string().trim().optional(),
  project_id: z.string().trim().optional(),
  primary_project_id: z.string().trim().optional(),
  project_ids: z.array(z.string().trim()).optional(),
  approval_mode: z.enum(['ask', 'trusted']).optional(),
  execution_mode: z.enum(['act', 'plan']).optional(),
  run_budget_mode: z.enum(['inherit', 'standard', 'extended', 'marathon', 'unbounded']).optional(),
  workspace_mode: z.enum(['shared_project_root', 'isolated_worktree', 'scratch_only']).optional(),
  branch_name: z.string().trim().optional()
});

export const updateSessionRequestSchema = z.object({
  title: z.string().trim().optional(),
  profile_id: z.string().trim().optional(),
  route_id: z.string().trim().optional(),
  provider: z.string().trim().optional(),
  model: z.string().trim().optional(),
  state: z.string().trim().optional(),
  project_id: z.string().trim().optional(),
  primary_project_id: z.string().trim().optional(),
  project_ids: z.array(z.string().trim()).optional(),
  approval_mode: z.enum(['ask', 'trusted']).optional(),
  execution_mode: z.enum(['act', 'plan']).optional(),
  run_budget_mode: z.enum(['inherit', 'standard', 'extended', 'marathon', 'unbounded']).optional(),
  workspace_mode: z.enum(['shared_project_root', 'isolated_worktree', 'scratch_only']).optional(),
  branch_name: z.string().trim().optional()
});

export const sessionPromptRequestSchema = z.object({
  prompt: z.string().default(''),
  images: z.array(sessionTurnImageSchema).default([]),
  role: z.enum(['main', 'utility']).default('main')
}).refine((value) => value.prompt.trim().length > 0 || value.images.length > 0, {
  message: 'A prompt or at least one image is required.'
});

export const approvalResolutionRequestSchema = z.object({
  note: z.string().trim().optional()
});

export const createPlaybookRequestSchema = z.object({
  title: z.string().trim().min(1),
  description: z.string().optional(),
  prompt: z.string().trim().min(1),
  profile_id: z.string().optional(),
  project_id: z.string().optional(),
  enabled: z.boolean().optional(),
  policy_bundle: z.string().trim().min(1),
  trigger_kind: z.string().trim().min(1),
  schedule_interval_secs: z.number().int().positive().optional(),
  event_kind: z.string().optional()
});

export const updatePlaybookRequestSchema = z.object({
  title: z.string().optional(),
  description: z.string().optional(),
  prompt: z.string().optional(),
  profile_id: z.string().optional(),
  project_id: z.string().optional(),
  enabled: z.boolean().optional(),
  policy_bundle: z.string().optional(),
  trigger_kind: z.string().optional(),
  schedule_interval_secs: z.number().int().positive().nullable().optional(),
  event_kind: z.string().nullable().optional()
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

export const workspaceModelConfigSchema = z.object({
  adapter: z.string().trim().min(1),
  model: z.string(),
  base_url: z.string().default(''),
  api_key: z.string().default('')
});

export const workspaceProfileSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  is_default: z.boolean(),
  main: workspaceModelConfigSchema,
  utility: workspaceModelConfigSchema,
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const workspaceProfileWriteRequestSchema = z.object({
  title: z.string().trim().min(1),
  main: workspaceModelConfigSchema,
  utility: workspaceModelConfigSchema,
  is_default: z.boolean().optional()
});

export const workspaceSummarySchema = z.object({
  root_path: z.string(),
  default_profile_id: z.string(),
  main_target: z.string(),
  utility_target: z.string(),
  run_budget: runBudgetSummarySchema,
  profiles: z.array(workspaceProfileSummarySchema),
  projects: z.array(projectSummarySchema)
});

export const workspaceUpdateRequestSchema = z.object({
  root_path: z.string().trim().min(1).optional(),
  default_profile_id: z.string().trim().min(1).optional(),
  main_target: z.string().trim().min(1).optional(),
  utility_target: z.string().trim().min(1).optional(),
  run_budget: runBudgetSummarySchema.optional()
});

export const projectUpdateRequestSchema = z.object({
  title: z.string().trim().optional()
});

export const vaultStatusSummarySchema = z.object({
  initialized: z.boolean(),
  locked: z.boolean(),
  state: z.string(),
  vault_id: z.string().default(''),
  cipher: z.string().default(''),
  kdf_algorithm: z.string().default(''),
  created_at: z.number().int().nullable().optional(),
  updated_at: z.number().int().nullable().optional()
});

export const vaultSecretSummarySchema = z.object({
  id: z.string(),
  scope_kind: z.string(),
  scope_id: z.string(),
  name: z.string(),
  description: z.string().default(''),
  configured: z.boolean(),
  version: z.number().int(),
  created_at: z.number().int(),
  updated_at: z.number().int(),
  last_used_at: z.number().int().nullable().optional()
});

export const vaultSecretListResponseSchema = z.object({
  secrets: z.array(vaultSecretSummarySchema).default([])
});

export const vaultSecretUpsertRequestSchema = z.object({
  id: z.string().optional(),
  scope_kind: z.string().default('workspace'),
  scope_id: z.string().default('workspace'),
  name: z.string().trim().min(1),
  description: z.string().default(''),
  secret: z.string().min(1)
});

export const vaultSecretUpdateRequestSchema = vaultSecretUpsertRequestSchema.partial().extend({
  secret: z.string().min(1)
});

export const vaultSecretPolicySummarySchema = z.object({
  id: z.string(),
  secret_id: z.string(),
  consumer_kind: z.string(),
  consumer_id: z.string(),
  permission: z.string(),
  approval_mode: z.string(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const vaultSecretPolicyListResponseSchema = z.object({
  policies: z.array(vaultSecretPolicySummarySchema).default([])
});

export const vaultSecretPolicyUpsertRequestSchema = z.object({
  id: z.string().optional(),
  consumer_kind: z.string().trim().min(1),
  consumer_id: z.string().trim().min(1),
  permission: z.string().trim().min(1),
  approval_mode: z.string().trim().min(1)
});

export const updateConfigRequestSchema = z.object({
  tracked_channel: z.string().trim().min(1).optional(),
  tracked_ref: z.string().trim().min(1).optional()
});

export const routeTargetSchema = z.object({
  provider: z.string(),
  model: z.string(),
  base_url: z.string().default(''),
  api_key: z.string().default('')
});

export const nucleusToolDescriptorSchema = z.object({
  id: z.string(),
  title: z.string(),
  description: z.string(),
  input_schema: z.unknown().default({}),
  source: z.string()
});

export const skillManifestSchema = z.object({
  id: z.string(),
  title: z.string(),
  description: z.string(),
  instructions: z.string().default(''),
  activation_mode: z.string(),
  triggers: z.array(z.string()).default([]),
  include_paths: z.array(z.string()).default([]),
  required_tools: z.array(z.string()).default([]),
  required_mcps: z.array(z.string()).default([]),
  project_filters: z.array(z.string()).default([]),
  enabled: z.boolean()
});

export const skillPackageRecordSchema = z.object({
  id: z.string(),
  name: z.string(),
  version: z.string(),
  manifest_json: z.unknown(),
  instructions: z.string(),
  source_kind: z.string().default('manual'),
  source_url: z.string().default(''),
  source_repo_url: z.string().default(''),
  source_owner: z.string().default(''),
  source_repo: z.string().default(''),
  source_ref: z.string().default(''),
  source_parent_path: z.string().default(''),
  source_skill_path: z.string().default(''),
  source_commit: z.string().default(''),
  imported_at: z.number().int().nullable().optional(),
  last_checked_at: z.number().int().nullable().optional(),
  latest_source_commit: z.string().default(''),
  update_status: z.string().default('unknown'),
  content_checksum: z.string().default(''),
  dirty_status: z.string().default('unknown'),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const skillPackageUpsertRequestSchema = z.object({
  id: z.string().optional(),
  name: z.string(),
  version: z.string(),
  manifest_json: z.unknown(),
  instructions: z.string(),
  source_kind: z.string().default('manual'),
  source_url: z.string().default(''),
  source_repo_url: z.string().default(''),
  source_owner: z.string().default(''),
  source_repo: z.string().default(''),
  source_ref: z.string().default(''),
  source_parent_path: z.string().default(''),
  source_skill_path: z.string().default(''),
  source_commit: z.string().default(''),
  content_checksum: z.string().default('')
});

export const skillImportRequestSchema = z.object({
  source: z.string(),
  scope_kind: z.string().default('workspace'),
  scope_id: z.string().default('default')
});

export const skillReconcileRequestSchema = z.object({
  skill_ids: z.array(z.string()).default([])
});

export const skillReconcileCandidateSchema = z.object({
  skill_id: z.string(),
  title: z.string(),
  path: z.string(),
  already_registered: z.boolean()
});

export const skillReconcileScanResponseSchema = z.object({
  skills_dir: z.string(),
  candidates: z.array(skillReconcileCandidateSchema).default([]),
  errors: z.array(z.string()).default([])
});

export const skillInstallVerificationSchema = z.object({
  files_copied: z.boolean(),
  manifest_registered: z.boolean(),
  package_registered: z.boolean(),
  installation_registered: z.boolean(),
  instructions_non_empty: z.boolean(),
  source_metadata_stored: z.boolean(),
  checksum_recorded: z.boolean()
});

export const skillInstallResultSchema = z.object({
  skill_id: z.string(),
  package_id: z.string(),
  installation_id: z.string(),
  source_kind: z.string(),
  source_url: z.string(),
  source_repo: z.string(),
  source_ref: z.string(),
  source_skill_path: z.string(),
  source_commit: z.string(),
  content_checksum: z.string(),
  dirty_status: z.string(),
  update_status: z.string(),
  status: z.string(),
  verification: skillInstallVerificationSchema
});

export const skillImportResponseSchema = z.object({
  installed: z.array(skillInstallResultSchema).default([]),
  errors: z.array(z.string()).default([])
});

export const skillInstallationRecordSchema = z.object({
  id: z.string(),
  package_id: z.string(),
  scope_kind: z.string(),
  scope_id: z.string(),
  enabled: z.boolean(),
  pinned_version: z.string().nullable().optional(),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const skillInstallationUpsertRequestSchema = z.object({
  id: z.string().optional(),
  package_id: z.string(),
  scope_kind: z.string(),
  scope_id: z.string(),
  enabled: z.boolean().optional(),
  pinned_version: z.string().nullable().optional()
});

export const mcpServerSummarySchema = z.object({
  id: z.string(),
  title: z.string(),
  enabled: z.boolean(),
  transport: z.string().default('stdio'),
  command: z.string().default(''),
  args: z.array(z.string()).default([]),
  env_json: z.unknown().default({}),
  url: z.string().default(''),
  headers_json: z.unknown().default({}),
  auth_kind: z.string().default('none'),
  auth_ref: z.string().default(''),
  sync_status: z.string().default('pending'),
  last_error: z.string().default(''),
  last_synced_at: z.number().int().nullable().default(null),
  tools: z.array(nucleusToolDescriptorSchema).default([]),
  resources: z.array(z.string()).default([])
});

export const mcpServerRecordSchema = z.object({
  id: z.string(),
  workspace_id: z.string().default('workspace'),
  title: z.string(),
  transport: z.string(),
  command: z.string().default(''),
  args: z.array(z.string()).default([]),
  env_json: z.unknown().default({}),
  url: z.string().default(''),
  headers_json: z.unknown().default({}),
  auth_kind: z.string().default('none'),
  auth_ref: z.string().default(''),
  enabled: z.boolean(),
  sync_status: z.string(),
  last_error: z.string().default(''),
  last_synced_at: z.number().int().nullable(),
  created_at: z.number().int().default(0),
  updated_at: z.number().int().default(0)
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
  repo_root: z.string().nullable(),
  daemon_bind: z.string(),
  install_kind: z.string(),
  restart_mode: z.string(),
  restart_supported: z.boolean()
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

export const localInterfaceSummarySchema = z.object({
  name: z.string(),
  address: z.string(),
  is_loopback: z.boolean(),
  is_private: z.boolean()
});

export const securityPostureSummarySchema = z.object({
  configured_bind: z.string(),
  exposure: z.string(),
  https_active: z.boolean(),
  current_origin: z.string().nullable(),
  current_origin_vault_safe: z.boolean(),
  current_origin_reason: z.string(),
  local_interfaces: z.array(localInterfaceSummarySchema).default([]),
  warnings: z.array(z.string()).default([])
});

export const compatibilitySummarySchema = z.object({
  server_version: z.string(),
  minimum_client_version: z.string().nullable(),
  minimum_server_version: z.string().nullable(),
  surface_version: z.string(),
  capability_flags: z.array(z.string())
});

export const updateStatusSchema = z.object({
  install_kind: z.string(),
  tracked_channel: z.string().nullable(),
  tracked_ref: z.string().nullable(),
  repo_root: z.string().nullable(),
  current_ref: z.string().nullable(),
  remote_name: z.string().nullable(),
  remote_url: z.string().nullable(),
  current_commit: z.string().nullable(),
  current_commit_short: z.string().nullable(),
  latest_commit: z.string().nullable(),
  latest_commit_short: z.string().nullable(),
  latest_version: z.string().nullable(),
  latest_release_id: z.string().nullable(),
  update_available: z.boolean(),
  dirty_worktree: z.boolean(),
  restart_required: z.boolean(),
  last_successful_check_at: z.number().int().nullable(),
  last_attempted_check_at: z.number().int().nullable(),
  last_attempt_result: z.string().nullable(),
  latest_error: z.string().nullable(),
  latest_error_at: z.number().int().nullable(),
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
  security: securityPostureSummarySchema,
  compatibility: compatibilitySummarySchema,
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

export const memoryEntrySchema = z.object({
  id: z.string(),
  scope_kind: z.string(),
  scope_id: z.string(),
  title: z.string(),
  content: z.string(),
  tags: z.array(z.string()),
  enabled: z.boolean(),
  status: z.string().default('accepted'),
  memory_kind: z.string().default('note'),
  source_kind: z.string().default('manual'),
  source_id: z.string().default(''),
  confidence: z.number().default(1),
  created_by: z.string().default('user'),
  last_used_at: z.number().int().nullable().optional(),
  use_count: z.number().int().default(0),
  supersedes_id: z.string().default(''),
  metadata_json: z.unknown().default({}),
  created_at: z.number().int(),
  updated_at: z.number().int()
});

export const memoryEntryUpsertRequestSchema = z.object({
  id: z.string().optional(),
  scope_kind: z.string(),
  scope_id: z.string(),
  title: z.string(),
  content: z.string(),
  tags: z.array(z.string()).default([]),
  enabled: z.boolean().optional(),
  status: z.string().optional(),
  memory_kind: z.string().optional(),
  source_kind: z.string().optional(),
  source_id: z.string().optional(),
  confidence: z.number().optional(),
  created_by: z.string().optional(),
  last_used_at: z.number().int().nullable().optional(),
  use_count: z.number().int().optional(),
  supersedes_id: z.string().optional(),
  metadata_json: z.unknown().optional()
});

export const memoryCandidateSchema = z.object({
  id: z.string(),
  scope_kind: z.string(),
  scope_id: z.string(),
  session_id: z.string().default(''),
  turn_id_start: z.string().default(''),
  turn_id_end: z.string().default(''),
  candidate_kind: z.string().default('note'),
  title: z.string(),
  content: z.string(),
  tags: z.array(z.string()).default([]),
  evidence: z.array(z.string()).default([]),
  reason: z.string().default(''),
  confidence: z.number().default(0),
  status: z.string().default('pending'),
  dedupe_key: z.string().default(''),
  accepted_memory_id: z.string().default(''),
  created_by: z.string().default('utility_worker'),
  created_at: z.number().int(),
  updated_at: z.number().int(),
  metadata_json: z.unknown().default({})
});

export const memoryCandidateUpsertRequestSchema = z.object({
  id: z.string().optional(),
  scope_kind: z.string(),
  scope_id: z.string(),
  session_id: z.string().optional(),
  turn_id_start: z.string().optional(),
  turn_id_end: z.string().optional(),
  candidate_kind: z.string().optional(),
  title: z.string(),
  content: z.string(),
  tags: z.array(z.string()).default([]),
  evidence: z.array(z.string()).default([]),
  reason: z.string().optional(),
  confidence: z.number().optional(),
  status: z.string().optional(),
  dedupe_key: z.string().optional(),
  accepted_memory_id: z.string().optional(),
  created_by: z.string().optional(),
  metadata_json: z.unknown().optional()
});

export const memoryCandidateListResponseSchema = z.object({
  candidates: z.array(memoryCandidateSchema).default([])
});

export const memorySummarySchema = z.object({
  entries: z.array(memoryEntrySchema),
  enabled_count: z.number().int(),
  scope_count: z.number().int()
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
  version: z.string(),
  compatibility: compatibilitySummarySchema
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
    event: z.literal('job.created'),
    data: jobSummarySchema
  }),
  z.object({
    event: z.literal('job.updated'),
    data: jobSummarySchema
  }),
  z.object({
    event: z.literal('worker.updated'),
    data: workerSummarySchema
  }),
  z.object({
    event: z.literal('approval.requested'),
    data: approvalRequestSummarySchema
  }),
  z.object({
    event: z.literal('approval.resolved'),
    data: approvalRequestSummarySchema
  }),
  z.object({
    event: z.literal('artifact.added'),
    data: artifactSummarySchema
  }),
  z.object({
    event: z.literal('command_session.updated'),
    data: commandSessionSummarySchema
  }),
  z.object({
    event: z.literal('job.completed'),
    data: jobSummarySchema
  }),
  z.object({
    event: z.literal('job.failed'),
    data: jobSummarySchema
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
export type PolicyDecisionSummary = z.infer<typeof policyDecisionSummarySchema>;
export type ToolCapabilitySummary = z.infer<typeof toolCapabilitySummarySchema>;
export type JobSummary = z.infer<typeof jobSummarySchema>;
export type WorkerSummary = z.infer<typeof workerSummarySchema>;
export type ToolCallSummary = z.infer<typeof toolCallSummarySchema>;
export type ApprovalRequestSummary = z.infer<typeof approvalRequestSummarySchema>;
export type ArtifactSummary = z.infer<typeof artifactSummarySchema>;
export type CommandSessionSummary = z.infer<typeof commandSessionSummarySchema>;
export type JobEvent = z.infer<typeof jobEventSchema>;
export type JobDetail = z.infer<typeof jobDetailSchema>;
export type PlaybookSummary = z.infer<typeof playbookSummarySchema>;
export type PlaybookDetail = z.infer<typeof playbookDetailSchema>;
export type PromptProgressUpdate = z.infer<typeof promptProgressUpdateSchema>;
export type ApprovalResolutionRequest = z.infer<typeof approvalResolutionRequestSchema>;
export type CreatePlaybookRequest = z.infer<typeof createPlaybookRequestSchema>;
export type UpdatePlaybookRequest = z.infer<typeof updatePlaybookRequestSchema>;
export type ActionSummary = z.infer<typeof actionSummarySchema>;
export type ActionRunResponse = z.infer<typeof actionRunResponseSchema>;
export type AuditEvent = z.infer<typeof auditEventSchema>;
export type ProjectSummary = z.infer<typeof projectSummarySchema>;
export type WorkspaceSummary = z.infer<typeof workspaceSummarySchema>;
export type WorkspaceModelConfig = z.infer<typeof workspaceModelConfigSchema>;
export type WorkspaceProfileSummary = z.infer<typeof workspaceProfileSummarySchema>;
export type RouterProfileSummary = z.infer<typeof routerProfileSummarySchema>;
export type RouteTarget = z.infer<typeof routeTargetSchema>;
export type NucleusToolDescriptor = z.infer<typeof nucleusToolDescriptorSchema>;
export type SkillManifest = z.infer<typeof skillManifestSchema>;
export type SkillPackageRecord = z.infer<typeof skillPackageRecordSchema>;
export type SkillPackageUpsertRequest = z.infer<typeof skillPackageUpsertRequestSchema>;
export type SkillImportRequest = z.infer<typeof skillImportRequestSchema>;
export type SkillReconcileRequest = z.infer<typeof skillReconcileRequestSchema>;
export type SkillReconcileCandidate = z.infer<typeof skillReconcileCandidateSchema>;
export type SkillReconcileScanResponse = z.infer<typeof skillReconcileScanResponseSchema>;
export type SkillInstallResult = z.infer<typeof skillInstallResultSchema>;
export type SkillImportResponse = z.infer<typeof skillImportResponseSchema>;
export type SkillInstallationRecord = z.infer<typeof skillInstallationRecordSchema>;
export type SkillInstallationUpsertRequest = z.infer<typeof skillInstallationUpsertRequestSchema>;
export type McpServerSummary = z.infer<typeof mcpServerSummarySchema>;
export type McpServerRecord = z.infer<typeof mcpServerRecordSchema>;
export type MemoryEntry = z.infer<typeof memoryEntrySchema>;
export type MemoryCandidate = z.infer<typeof memoryCandidateSchema>;
export type MemoryCandidateUpsertRequest = z.infer<typeof memoryCandidateUpsertRequestSchema>;
export type MemoryEntryUpsertRequest = z.infer<typeof memoryEntryUpsertRequestSchema>;
export type MemorySummary = z.infer<typeof memorySummarySchema>;
export type VaultStatusSummary = z.infer<typeof vaultStatusSummarySchema>;
export type VaultSecretSummary = z.infer<typeof vaultSecretSummarySchema>;
export type VaultSecretPolicySummary = z.infer<typeof vaultSecretPolicySummarySchema>;
export type VaultSecretUpsertRequest = z.infer<typeof vaultSecretUpsertRequestSchema>;
export type VaultSecretPolicyUpsertRequest = z.infer<typeof vaultSecretPolicyUpsertRequestSchema>;
export type SystemStats = z.infer<typeof systemStatsSchema>;
export type ProcessListResponse = z.infer<typeof processListResponseSchema>;
export type ProcessSnapshot = z.infer<typeof processSnapshotSchema>;
export type DiskStat = z.infer<typeof diskStatSchema>;
export type SettingsSummary = z.infer<typeof settingsSummarySchema>;
export type CompatibilitySummary = z.infer<typeof compatibilitySummarySchema>;
export type UpdateStatus = z.infer<typeof updateStatusSchema>;
export type UpdateConfigRequest = z.infer<typeof updateConfigRequestSchema>;
export type DaemonEvent = z.infer<typeof daemonEventSchema>;
