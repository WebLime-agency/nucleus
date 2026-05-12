<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { formatDateTime, formatState } from '$lib/nucleus/format';
  import type { JobSummary } from '$lib/nucleus/schemas';

  let {
    jobs,
    selectedJobId = '',
    emptyMessage = 'No jobs have been queued yet.',
    onSelect,
    badgeVariantForJobState
  }: {
    jobs: JobSummary[];
    selectedJobId?: string;
    emptyMessage?: string;
    onSelect: (jobId: string) => void;
    badgeVariantForJobState: (state: string) => 'default' | 'secondary' | 'warning' | 'destructive';
  } = $props();
</script>

{#if jobs.length === 0}
  <div class="rounded-xl border border-dashed border-zinc-800 bg-zinc-950/60 px-4 py-5 text-sm text-zinc-500">
    {emptyMessage}
  </div>
{:else}
  {#each jobs as job}
    <button
      type="button"
      class={`w-full rounded-xl border px-4 py-3 text-left transition ${
        selectedJobId === job.id
          ? 'border-lime-400/40 bg-lime-400/10'
          : 'border-zinc-800 bg-zinc-950/60 hover:border-zinc-700'
      }`}
      onclick={() => onSelect(job.id)}
    >
      <div class="flex items-start justify-between gap-3">
        <div>
          <div class="text-sm font-medium text-zinc-100">{job.title}</div>
          <div class="mt-1 text-xs text-zinc-500">{job.prompt_excerpt}</div>
        </div>
        <Badge variant={badgeVariantForJobState(job.state)}>
          {formatState(job.state)}
        </Badge>
      </div>
      <div class="mt-3 flex flex-wrap gap-2 text-[11px] text-zinc-500">
        <span>{formatState(job.trigger_kind)}</span>
        <span>{job.pending_approval_count} approvals</span>
        <span>{job.artifact_count} artifacts</span>
        <span>{formatDateTime(job.updated_at)}</span>
      </div>
    </button>
  {/each}
{/if}
