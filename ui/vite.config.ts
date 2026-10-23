/// <reference types="vitest/config" />
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  // Static SPA served from a private S3 bucket through CloudFront. Client routes fall back to the
  // generated shell; production API calls use the same-origin /api behavior configured in CDK.
  plugins: [sveltekit({ adapter: adapter({ fallback: 'index.html' }) })],
  // Production uses same-origin /api through CloudFront. This override is only needed when the
  // UI and backend run on different origins during local development.
  envPrefix: 'PUBLIC_',
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
