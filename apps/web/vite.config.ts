import { defineConfig, loadEnv } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '');
  const webPort = Number(env.NUCLEUS_WEB_PORT || '5300');
  const daemonOrigin = env.NUCLEUS_DAEMON_ORIGIN || 'http://127.0.0.1:5299';
  const allowReservedPort = env.NUCLEUS_ALLOW_RESERVED_WEB_PORT === '1';

  if (webPort === 5201 && !allowReservedPort) {
    throw new Error(
      'Port 5201 is reserved for the official managed-release Nucleus instance on this host. ' +
        'Use the source-dev defaults NUCLEUS_WEB_PORT=5300 and NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:5299, ' +
        'or set NUCLEUS_ALLOW_RESERVED_WEB_PORT=1 only for intentional diagnostics.'
    );
  }

  const daemonWsOrigin = daemonOrigin.startsWith('https://')
    ? daemonOrigin.replace(/^https:\/\//, 'wss://')
    : daemonOrigin.replace(/^http:\/\//, 'ws://');

  return {
    plugins: [tailwindcss(), sveltekit()],
    server: {
      host: '::',
      port: webPort,
      strictPort: true,
      allowedHosts: ['mini-server', '.ts.net'],
      proxy: {
        '/api': daemonOrigin,
        '/health': daemonOrigin,
        '/ws': {
          target: daemonWsOrigin,
          ws: true
        }
      }
    }
  };
});
