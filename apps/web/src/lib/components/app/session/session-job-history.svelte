<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import type {
    ApprovalRequestSummary,
    JobDetail,
    JobSummary,
    ToolCallSummary
  } from '$lib/nucleus/schemas';

  let {
    jobLoading,
    jobSummaries,
    jobDetail,
    selectedJobId,
    approvalActioningId,
    onSelectJob,
    onApprove,
    onDeny,
    badgeVariantForJobState,
    badgeVariantForToolCall,
    formatApprovalTitle,
    formatApprovalDetail,
    formatCommandSessionSummary
  }: {
    jobLoading: boolean;
    jobSummaries: JobSummary[];
    jobDetail: JobDetail | null;
    selectedJobId: string;
    approvalActioningId: string | null;
    onSelectJob: (jobId: string) => void;
    onApprove: (approval: ApprovalRequestSummary) => void;
    onDeny: (approval: ApprovalRequestSummary) => void;
    badgeVariantForJobState: (state: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    badgeVariantForToolCall: (state: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    formatApprovalTitle: (approval: ApprovalRequestSummary, toolCalls: ToolCallSummary[]) => string;
    formatApprovalDetail: (approval: ApprovalRequestSummary, toolCalls?: ToolCallSummary[]) => string;
    formatCommandSessionSummary: (commandSession: JobDetail['command_sessions'][number]) => string;
  } = $props();
</script>

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
          class={`w-full rounded-xl border px-4 py-3 text-left transition ${
            selectedJobId === job.id
              ? 'border-lime-400/40 bg-lime-400/10'
              : 'border-zinc-800 bg-zinc-900/75 hover:border-zinc-700'
          }`}
          onclick={() => onSelectJob(job.id)}
        >
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="truncate text-sm font-medium text-zinc-100">{job.title}</div>
              <div class="mt-1 text-xs text-zinc-500">
                {job.result_summary || job.prompt_excerpt || job.purpose}
              </div>
            </div>
            <Badge variant={badgeVariantForJobState(job.state)}>{formatState(job.state)}</Badge>
          </div>
          <div class="mt-3 flex flex-wrap gap-2 text-[11px] text-zinc-500">
            <span>{job.pending_approval_count} approvals</span>
            <span>{job.artifact_count} artifacts</span>
            <span>{formatDateTime(job.updated_at)}</span>
          </div>
        </button>
      {/each}
    </div>

    {#if jobDetail}
      <div class="rounded-2xl border border-zinc-800 bg-zinc-900/75 p-4">
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="truncate text-base font-medium text-zinc-100">{jobDetail.job.title}</div>
            <div class="mt-1 text-sm text-zinc-500">
              {jobDetail.job.result_summary || jobDetail.job.prompt_excerpt || jobDetail.job.purpose}
            </div>
          </div>
          <Badge variant={badgeVariantForJobState(jobDetail.job.state)}>{formatState(jobDetail.job.state)}</Badge>
        </div>

        <div class="mt-4 grid gap-4 lg:grid-cols-2">
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
                      <Badge variant={badgeVariantForJobState(approval.state)}>{formatState(approval.state)}</Badge>
                    </div>
                    {#if approval.state === 'pending'}
                      <div class="mt-3 flex flex-wrap gap-2">
                        <button class="rounded-md border border-zinc-700 px-2 py-1 text-xs text-zinc-100" disabled={approvalActioningId !== null} onclick={() => onApprove(approval)}>
                          {approvalActioningId === approval.id ? 'Approving' : 'Approve'}
                        </button>
                        <button class="rounded-md border border-zinc-700 px-2 py-1 text-xs text-zinc-100" disabled={approvalActioningId !== null} onclick={() => onDeny(approval)}>
                          {approvalActioningId === approval.id ? 'Resolving' : 'Deny'}
                        </button>
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
                      <Badge variant={badgeVariantForToolCall(commandSession.state)}>{formatState(commandSession.state)}</Badge>
                    </div>
                    <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-zinc-600">
                      <span>{commandSession.mode}</span>
                      <span>{compactPath(commandSession.cwd)}</span>
                      <span>{commandSession.output_limit_bytes} byte cap</span>
                      <span>{commandSession.timeout_secs}s timeout</span>
                    </div>
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
