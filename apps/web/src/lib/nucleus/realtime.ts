import { browser } from '$app/environment';

import { readAccessToken } from './auth';
import { daemonEventSchema, type DaemonEvent } from './schemas';

export type StreamStatus = 'connecting' | 'connected' | 'reconnecting' | 'closed';

interface StreamOptions {
  onEvent: (event: DaemonEvent) => void;
  onStatusChange?: (status: StreamStatus) => void;
  onError?: (message: string) => void;
  onAuthError?: (message: string) => void;
}

export function connectDaemonStream(options: StreamOptions) {
  if (!browser) {
    return () => {};
  }

  let disposed = false;
  let socket: WebSocket | null = null;
  let reconnectTimer: number | null = null;
  let reconnectDelayMs = 1_000;

  const setStatus = (status: StreamStatus) => {
    options.onStatusChange?.(status);
  };

  const clearReconnectTimer = () => {
    if (reconnectTimer !== null) {
      window.clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  };

  const scheduleReconnect = () => {
    clearReconnectTimer();
    reconnectTimer = window.setTimeout(() => {
      if (!disposed) {
        connect();
      }
    }, reconnectDelayMs);
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, 10_000);
  };

  const connect = () => {
    clearReconnectTimer();
    setStatus(socket === null ? 'connecting' : 'reconnecting');

    const token = readAccessToken();
    if (!token) {
      setStatus('closed');
      return;
    }

    const url = new URL('/ws', window.location.href);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    url.searchParams.set('token', token);

    socket = new WebSocket(url);

    socket.onopen = () => {
      reconnectDelayMs = 1_000;
      setStatus('connected');
    };

    socket.onmessage = (message) => {
      if (typeof message.data !== 'string') {
        return;
      }

      let payload: unknown;

      try {
        payload = JSON.parse(message.data);
      } catch {
        options.onError?.('Nucleus stream sent invalid JSON.');
        return;
      }

      const parsed = daemonEventSchema.safeParse(payload);

      if (!parsed.success) {
        options.onError?.('Nucleus stream sent an invalid event payload.');
        return;
      }

      options.onEvent(parsed.data);
    };

    socket.onerror = () => {
      socket?.close();
    };

    socket.onclose = (event) => {
      socket = null;

      if (disposed) {
        setStatus('closed');
        return;
      }

      if (event.code === 4401) {
        setStatus('closed');
        options.onAuthError?.(
          event.reason || 'Authentication required. Enter a valid Nucleus access token.'
        );
        return;
      }

      setStatus('reconnecting');
      scheduleReconnect();
    };
  };

  connect();

  return () => {
    disposed = true;
    clearReconnectTimer();
    socket?.close();
    socket = null;
    setStatus('closed');
  };
}
