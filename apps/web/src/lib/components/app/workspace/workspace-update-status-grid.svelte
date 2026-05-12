<script lang="ts">
  import { formatDateTime, formatLatestTargetLabel, formatState } from '$lib/nucleus/format';

  let {
    latestTargetLabel,
    currentVersionLabel,
    updateStateLabel,
    lastSuccessfulCheckAt,
    lastAttemptedCheckAt,
    lastAttemptResult
  }: {
    latestTargetLabel: string;
    currentVersionLabel: string;
    updateStateLabel: string;
    lastSuccessfulCheckAt: number | null;
    lastAttemptedCheckAt: number | null;
    lastAttemptResult: string | null;
  } = $props();
</script>

<div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Latest Target</div>
    <div class="mt-2 text-sm font-medium text-zinc-50">{latestTargetLabel}</div>
  </div>
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Current Version</div>
    <div class="mt-2 text-sm font-medium text-zinc-50">{currentVersionLabel}</div>
  </div>
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Update State</div>
    <div class="mt-2 text-sm font-medium text-zinc-50">{updateStateLabel}</div>
  </div>
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Last Successful Check</div>
    <div class="mt-2 text-sm font-medium text-zinc-50">
      {lastSuccessfulCheckAt ? formatDateTime(lastSuccessfulCheckAt) : 'Never'}
    </div>
  </div>
  <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
    <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Last Attempted Check</div>
    <div class="mt-2 text-sm font-medium text-zinc-50">
      {lastAttemptedCheckAt ? formatDateTime(lastAttemptedCheckAt) : 'Never'}
    </div>
    <div class="mt-1 text-xs text-zinc-500">
      {lastAttemptResult ? formatState(lastAttemptResult) : 'No attempts yet'}
    </div>
  </div>
</div>
