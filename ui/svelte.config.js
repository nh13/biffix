import adapter from '@sveltejs/adapter-static';

// Static SPA served from a private S3 bucket through CloudFront. Client routes fall back to the
// generated shell; production API calls use the same-origin /api behavior configured in CDK.
export default {
  kit: {
    adapter: adapter({ fallback: 'index.html' }),
  },
};
