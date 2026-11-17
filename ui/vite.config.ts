/// <reference types="vitest/config" />
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { workspaceVersion } from './src/lib/util/workspaceVersion';

// The release version, shown on the API docs page (see workspaceVersion).
const version = workspaceVersion(readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8'));

export default defineConfig({
  // Static SPA served from a private S3 bucket through CloudFront. Client routes fall back to the
  // generated shell; production API calls use the same-origin /api behavior configured in CDK.
  plugins: [sveltekit({ adapter: adapter({ fallback: 'index.html' }) })],
  // Production uses same-origin /api through CloudFront. This override is only needed when the
  // UI and backend run on different origins during local development.
  envPrefix: 'PUBLIC_',
  define: { __BIFFIX_VERSION__: JSON.stringify(version) },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
