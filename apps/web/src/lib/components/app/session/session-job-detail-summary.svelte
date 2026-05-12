<script lang="ts">
  import { CalendarClock, Play } from 'lucide-svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { compactPath, formatDateTime, formatState } from '$lib/nucleus/format';
  import type { JobDetail } from '$lib/nucleus/schemas';

  let {
    jobDetail,
    badgeVariantForJobState
  }: {
    jobDetail: JobDetail;
    badgeVariantForJobState: (state: string) => 'default' | 'secondary' | 'warning' | 'destructive';
  } = $props();
</script>

<div class="space-y-5">
  <div class="grid gap-4 sm:grid-cols-3">
    <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
      <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Approvals</div>
      <div class="mt-2 text-2xl font-semibold text-zinc-100">
        {jobDetail.job.pending_approval_count}
      </div>
    </div>
    <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
      <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Artifacts</div>
      <div class="mt-2 text-2xl font-semibold text-zinc-100">{jobDetail.job.artifact_count}</div>
    </div>
    <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
      <div class="text-xs uppercase tracking-[0.18em] text-zinc-500">Updated</div>
      <div class="mt-2 text-sm text-zinc-200">{formatDateTime(jobDetail.job.updated_at)}</div>
    </div>
  </div>

  <div class="space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
      <CalendarClock class="size-4" />
      Recent activity
    </div>
    {#if jobDetail.events.length === 0}
      <div class="text-sm text-zinc-500">No events were recorded for this job.</div>
    {:else}
      <div class="space-y-3">
        {#each [...jobDetail.events].reverse().slice(0, 8) as event}
          <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
            <div class="flex items-start justify-between gap-3">
              <div>
                <div class="text-sm font-medium text-zinc-100">{event.summary}</div>
                {#if event.detail}
                  <div class="mt-1 text-xs leading-5 text-zinc-500">{event.detail}</div>
                {/if}
              </div>
              <Badge variant={badgeVariantForJobState(event.status)}>
                {formatState(event.status)}
              </Badge>
            </div>
            <div class="mt-2 text-[11px] text-zinc-600">
              {event.event_type} · {formatDateTime(event.created_at)}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
      <CalendarClock class="size-4" />
      Approvals
    </div>
    {#if jobDetail.approvals.length === 0}
      <div class="text-sm text-zinc-500">No approvals were recorded for this job.</div>
    {:else}
      <div class="space-y-3">
        {#each [...jobDetail.approvals].reverse().slice(0, 4) as approval}
          <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
            <div class="flex items-start justify-between gap-3">
              <div>
                <div class="text-sm font-medium text-zinc-100">{approval.summary}</div>
                <div class="mt-1 text-xs leading-5 text-zinc-500">{approval.detail}</div>
              </div>
              <Badge variant={badgeVariantForJobState(approval.state)}>
                {formatState(approval.state)}
              </Badge>
            </div>
            {#if approval.diff_preview}
              <pre class="mt-3 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{approval.diff_preview}</pre>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-zinc-200">
      <Play class="size-4" />
      Artifacts
    </div>
    {#if jobDetail.artifacts.length === 0}
      <div class="text-sm text-zinc-500">No artifacts were recorded for this job.</div>
    {:else}
      <div class="space-y-3">
        {#each [...jobDetail.artifacts].reverse().slice(0, 4) as artifact}
          <div class="rounded-xl border border-zinc-800 bg-zinc-950/60 px-4 py-3">
            <div class="flex items-start justify-between gap-3">
              <div>
                <div class="text-sm font-medium text-zinc-100">{artifact.title}</div>
                <div class="mt-1 text-xs text-zinc-500">
                  {artifact.kind} · {formatDateTime(artifact.created_at)}
                </div>
              </div>
              <div class="text-[11px] text-zinc-600">{artifact.size_bytes} bytes</div>
            </div>
            {#if artifact.preview_text}
              <pre class="mt-3 overflow-x-auto whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-xs leading-5 text-zinc-500">{artifact.preview_text}</pre>
            {/if}
            <div class="mt-2 text-[11px] text-zinc-600">{compactPath(artifact.path)}</div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
