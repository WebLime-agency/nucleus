import { defineConfig, loadEnv } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '');
  const webPort = Number(env.NUCLEUS_WEB_PORT || '5201');
  const daemonOrigin = env.NUCLEUS_DAEMON_ORIGIN || 'http://127.0.0.1:42240';
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
