import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
  // Production uses same-origin /api through CloudFront. This override is only needed when the
  // UI and backend run on different origins during local development.
  envPrefix: 'PUBLIC_',
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
