import { formatState } from './format';
import type { RouterProfileSummary, RuntimeSummary } from './schemas';

export interface WorkspaceTargetOption {
  value: string;
  label: string;
  helper: string;
  kind: 'route' | 'provider';
  routeId?: string;
  provider?: string;
}

export interface WorkspaceTargetSummary {
  value: string;
  label: string;
  helper: string;
  kind: 'route' | 'provider' | 'unknown';
}

export function buildWorkspaceTargetOptions(
  routerProfiles: RouterProfileSummary[],
  runtimes: RuntimeSummary[]
): WorkspaceTargetOption[] {
  const routeOptions = routerProfiles.map((profile) => ({
    value: `route:${profile.id}`,
    label: profile.title,
    helper: profile.targets.map((target) => describeTarget(target.provider, target.model)).join(' -> '),
    kind: 'route' as const,
    routeId: profile.id
  }));

  const providerOptions = runtimes
    .filter((runtime) => runtime.supports_sessions)
    .map((runtime) => ({
      value: `provider:${runtime.id}`,
      label: `Direct ${formatState(runtime.id)}`,
      helper: runtime.default_model || runtime.summary,
      kind: 'provider' as const,
      provider: runtime.id
    }));

  return [...routeOptions, ...providerOptions];
}

export function parseWorkspaceTargetValue(value: string): {
  route_id?: string;
  provider?: string;
} {
  if (value.startsWith('route:')) {
    return { route_id: value.slice('route:'.length) };
  }

  if (value.startsWith('provider:')) {
    return { provider: value.slice('provider:'.length) };
  }

  return {};
}

export function describeWorkspaceTarget(
  value: string,
  routerProfiles: RouterProfileSummary[],
  runtimes: RuntimeSummary[]
): WorkspaceTargetSummary {
  const match = buildWorkspaceTargetOptions(routerProfiles, runtimes).find(
    (option) => option.value === value
  );

  if (match) {
    return match;
  }

  return {
    value,
    label: value || 'Unconfigured',
    helper: 'Unknown target selector',
    kind: 'unknown'
  };
}

function describeTarget(provider: string, model: string): string {
  if (!model) {
    return formatState(provider);
  }

  return `${formatState(provider)} ${model}`;
}
