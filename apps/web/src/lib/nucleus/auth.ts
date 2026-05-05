import { browser } from '$app/environment';

const ACCESS_TOKEN_STORAGE_KEY = 'nucleus.accessToken';

export class NucleusAuthError extends Error {
  constructor(message = 'Authentication required.') {
    super(message);
    this.name = 'NucleusAuthError';
  }
}

export function isAuthError(value: unknown): value is NucleusAuthError {
  return value instanceof NucleusAuthError;
}

export function readAccessToken() {
  if (!browser) {
    return '';
  }

  return window.localStorage.getItem(ACCESS_TOKEN_STORAGE_KEY)?.trim() ?? '';
}

export function writeAccessToken(token: string) {
  if (!browser) {
    return;
  }

  const normalized = token.trim();

  if (!normalized) {
    window.localStorage.removeItem(ACCESS_TOKEN_STORAGE_KEY);
    return;
  }

  window.localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, normalized);
}

export function clearAccessToken() {
  if (!browser) {
    return;
  }

  window.localStorage.removeItem(ACCESS_TOKEN_STORAGE_KEY);
}
