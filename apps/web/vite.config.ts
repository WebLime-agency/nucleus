import { defineConfig, loadEnv } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '');
  const webPort = Number(env.NUCLEUS_WEB_PORT || '5201');
  const daemonOrigin = env.NUCLEUS_DAEMON_ORIGIN || 'http://127.0.0.1:42240';
  const allowEbaProxy = env.NUCLEUS_ALLOW_EBA_PROXY === '1';
  const daemonUrl = new URL(daemonOrigin);
  const pointsAtEbaDaemon =
    daemonUrl.port === '5202' &&
    ['127.0.0.1', 'localhost', '0.0.0.0', 'mini-server'].includes(daemonUrl.hostname);

  if (webPort === 5201 && pointsAtEbaDaemon && !allowEbaProxy) {
    throw new Error(
      'Refusing to start the official/dev web UI against the EBA managed daemon on 5202. ' +
        'Use NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:42240 for source-checkout testing, ' +
        'or set NUCLEUS_ALLOW_EBA_PROXY=1 for an intentional diagnostic check.'
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
