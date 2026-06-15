import type { LocalJobSummary } from './schemas';

export type BadgeVariant = 'default' | 'secondary' | 'warning' | 'destructive';

export function localJobBadgeVariant(job: LocalJobSummary): BadgeVariant {
  if (localJobLastRunFailed(job)) return 'destructive';
  if (job.active_state === 'failed') return 'destructive';
  if (job.active_state === 'active') return 'default';
  if (job.enabled) return 'warning';
  return 'secondary';
}

export function localJobLastRunFailed(job: LocalJobSummary): boolean {
  const result = job.last_exit.result.trim();
  if (!result || result === 'unknown') return false;
  return result !== 'success';
}

export function localJobCanToggle(job: LocalJobSummary): boolean {
  return job.manageable;
}

export function localJobCanRun(job: LocalJobSummary): boolean {
  return job.triggered_unit.trim().length > 0;
}
