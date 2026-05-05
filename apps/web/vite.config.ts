import { defineConfig } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

export default defineConfig({
  plugins: [tailwindcss(), sveltekit()],
  server: {
    host: '::',
    port: 5201,
    strictPort: true,
    allowedHosts: ['mini-server', '.ts.net'],
    proxy: {
      '/api': 'http://127.0.0.1:42240',
      '/health': 'http://127.0.0.1:42240',
      '/ws': {
        target: 'ws://127.0.0.1:42240',
        ws: true
      }
    }
  }
});
