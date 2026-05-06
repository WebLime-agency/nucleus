import type { CompatibilitySummary } from './schemas';

export const CURRENT_CLIENT_VERSION = '0.1.0';
export const CURRENT_CLIENT_SURFACE_VERSION = '2026-05-managed-release-v1';

export type CompatibilityLevel = 'supported' | 'degraded' | 'blocked';

export type CompatibilityEvaluation = {
  level: CompatibilityLevel;
  message: string | null;
};

const REQUIRED_CAPABILITY_FLAGS = [
  'daemon-owned-update-state',
  'embedded-web-build',
  'install-kind-contract'
];

export function evaluateCompatibility(
  summary: CompatibilitySummary | null
): CompatibilityEvaluation {
  if (!summary) {
    return {
      level: 'degraded',
      message: 'Compatibility metadata has not been received from the daemon yet.'
    };
  }

  if (summary.surface_version !== CURRENT_CLIENT_SURFACE_VERSION) {
    return {
      level: 'blocked',
      message: `This client expects surface ${CURRENT_CLIENT_SURFACE_VERSION}, but the daemon reports ${summary.surface_version}. Reconnect with a matching client build before relying on full support.`
    };
  }

  if (
    summary.minimum_client_version &&
    compareVersions(CURRENT_CLIENT_VERSION, summary.minimum_client_version) < 0
  ) {
    return {
      level: 'blocked',
      message: `This client is ${CURRENT_CLIENT_VERSION}, but the daemon requires client ${summary.minimum_client_version} or newer. Update Nucleus before continuing.`
    };
  }

  if (
    summary.minimum_server_version &&
    compareVersions(summary.server_version, summary.minimum_server_version) < 0
  ) {
    return {
      level: 'blocked',
      message: `This daemon is ${summary.server_version}, but this client flow requires server ${summary.minimum_server_version} or newer. Update Nucleus before continuing.`
    };
  }

  const missingCapabilities = REQUIRED_CAPABILITY_FLAGS.filter(
    (flag) => !summary.capability_flags.includes(flag)
  );

  if (missingCapabilities.length > 0) {
    return {
      level: 'degraded',
      message: `The daemon did not advertise required capabilities: ${missingCapabilities.join(', ')}. Update and restart Nucleus before relying on update controls.`
    };
  }

  return {
    level: 'supported',
    message: null
  };
}

export function describeCompatibilityWarning(summary: CompatibilitySummary | null): string | null {
  return evaluateCompatibility(summary).message;
}

export function isCompatibilityBlocked(summary: CompatibilitySummary | null): boolean {
  return evaluateCompatibility(summary).level === 'blocked';
}

function compareVersions(left: string, right: string): number {
  const leftParts = parseVersion(left);
  const rightParts = parseVersion(right);
  const maxLength = Math.max(leftParts.length, rightParts.length);

  for (let index = 0; index < maxLength; index += 1) {
    const leftValue = leftParts[index] ?? 0;
    const rightValue = rightParts[index] ?? 0;

    if (leftValue > rightValue) return 1;
    if (leftValue < rightValue) return -1;
  }

  return 0;
}

function parseVersion(value: string): number[] {
  return value
    .trim()
    .replace(/^v/i, '')
    .split(/[+-]/)[0]
    .split('.')
    .map((part) => Number.parseInt(part, 10))
    .filter((part) => Number.isFinite(part));
}
