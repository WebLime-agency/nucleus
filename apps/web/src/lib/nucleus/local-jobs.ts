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
  return job.triggered_unit.trim().endsWith('.service');
}

export function localJobMatchesAllowlistGlob(pattern: string, unit: string): boolean {
  const glob = pattern.trim();
  if (!glob) return false;

  const table = Array.from({ length: glob.length + 1 }, () =>
    Array.from({ length: unit.length + 1 }, () => false)
  );
  table[0][0] = true;

  for (let index = 1; index <= glob.length; index += 1) {
    if (glob[index - 1] === '*') {
      table[index][0] = table[index - 1][0];
    }
  }

  for (let patternIndex = 1; patternIndex <= glob.length; patternIndex += 1) {
    for (let unitIndex = 1; unitIndex <= unit.length; unitIndex += 1) {
      const token = glob[patternIndex - 1];
      table[patternIndex][unitIndex] =
        token === '*'
          ? table[patternIndex - 1][unitIndex] || table[patternIndex][unitIndex - 1]
          : (token === '?' || token === unit[unitIndex - 1]) &&
            table[patternIndex - 1][unitIndex - 1];
    }
  }

  return table[glob.length][unit.length];
}

export function localJobHasLiteralAllowlistEntry(job: LocalJobSummary, allowlist: string[]): boolean {
  return allowlist.includes(job.unit);
}

export function localJobHasNonLiteralAllowlistMatch(job: LocalJobSummary, allowlist: string[]): boolean {
  return allowlist.some((glob) => glob !== job.unit && localJobMatchesAllowlistGlob(glob, job.unit));
}

export function localJobCanRemoveLiteralAllowlistEntry(job: LocalJobSummary, allowlist: string[]): boolean {
  return localJobHasLiteralAllowlistEntry(job, allowlist) && !localJobHasNonLiteralAllowlistMatch(job, allowlist);
}
