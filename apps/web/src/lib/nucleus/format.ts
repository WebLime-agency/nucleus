import type { UpdateStatus } from './schemas';

export function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B';

  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;

  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }

  return `${value.toFixed(unit >= 3 ? 1 : 0)} ${units[unit]}`;
}

export function formatPercent(value: number): string {
  return `${value.toFixed(1)}%`;
}

export function formatCount(value: number): string {
  return new Intl.NumberFormat().format(value);
}

export function formatClock(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit'
  }).format(timestamp);
}

export function formatDateTime(timestampSeconds: number): string {
  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit'
  }).format(timestampSeconds * 1000);
}

export function formatState(value: string): string {
  return value
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (character) => character.toUpperCase());
}

export function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

export function compactPath(path: string): string {
  return path.replace(/^\/home\/eba\/?/, '') || path;
}

type UpdateTargetLike = Pick<
  UpdateStatus,
  'install_kind' | 'latest_version' | 'latest_release_id' | 'latest_commit_short' | 'latest_commit'
>;

export function formatLatestTargetLabel(
  update: UpdateTargetLike | null | undefined,
  fallback: string
): string {
  if (!update) {
    return fallback;
  }

  if (update.install_kind === 'managed_release') {
    const version = update.latest_version?.trim();
    const releaseId = update.latest_release_id?.trim();

    if (version && releaseId) {
      return `${version} (${compactReleaseId(releaseId)})`;
    }

    if (version) {
      return version;
    }

    if (releaseId) {
      return releaseId;
    }
  }

  return update.latest_version ?? update.latest_commit_short ?? update.latest_commit ?? fallback;
}

function compactReleaseId(releaseId: string): string {
  const parts = releaseId.split('-');

  if (parts.length >= 2) {
    return parts.slice(-2).join('-');
  }

  return releaseId;
}
