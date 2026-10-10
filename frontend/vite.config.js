import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { functionsMixins } from 'vite-plugin-functions-mixins';

export default defineConfig({
  plugins: [functionsMixins({ deps: ['m3-svelte'] }), svelte()],
  clearScreen: false,
  base: './',
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: 'esnext',
    outDir: 'dist',
    emptyOutDir: true,
  },
});
