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
