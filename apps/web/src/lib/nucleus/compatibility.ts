import type { CompatibilitySummary } from './schemas';

export const CURRENT_CLIENT_SURFACE_VERSION = '2026-05-managed-release-v1';

export function describeCompatibilityWarning(summary: CompatibilitySummary | null): string | null {
  if (!summary) {
    return null;
  }

  if (summary.surface_version !== CURRENT_CLIENT_SURFACE_VERSION) {
    return `This client expects surface ${CURRENT_CLIENT_SURFACE_VERSION}, but the daemon reports ${summary.surface_version}. Reconnect with a matching client build before relying on full support.`;
  }

  return null;
}
