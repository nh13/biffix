// Deep links into the AWS console for resources a run references. Regional consoles take the
// region explicitly — the deploy's, from `GET /auth/config` (see `deploy.svelte.ts`) — and a link is
// left out (null) without one rather than pointed at a guessed region. A Batch log stream is in the
// fleet-wide `/aws/batch/job` unless the API names another group (a run's tasks log to their
// project's group: `TaskPage.log_group`).
const BATCH_LOG_GROUP = '/aws/batch/job';

/** An AWS region name (`us-east-1`, `eu-west-2`, `us-gov-west-1`, …). */
const REGION_RE = /^[a-z]{2}(-[a-z]+)+-\d+$/;

function validRegion(region: string | null | undefined): string | null {
  return region && REGION_RE.test(region) ? region : null;
}

/** The AWS console's host for a region's partition: GovCloud and China have their own. */
export function consoleHost(region: string): string {
  if (region.startsWith('us-gov-')) return 'console.amazonaws-us-gov.com';
  if (region.startsWith('cn-')) return 'console.amazonaws.cn';
  return 'console.aws.amazon.com';
}

/** The console's origin in a (validated) region: `https://<region>.<partition's console host>`. */
function regionalConsole(rgn: string): string {
  return `https://${rgn}.${consoleHost(rgn)}`;
}

/**
 * S3 console at a given `s3://bucket/prefix` URI (folder view). The console resolves a bucket's own
 * region, which need not be the deploy's (a project may bring an existing bucket from another
 * region); `region` (the deploy's) only picks the partition's console (GovCloud, China).
 */
export function s3ConsoleUrl(
  uri: string | null | undefined,
  region?: string | null,
): string | null {
  if (!uri) return null;
  const m = /^s3:\/\/([^/]+)\/?(.*)$/.exec(uri);
  if (!m) return null;
  const [, bucket, prefix] = m;
  const host = consoleHost(validRegion(region) ?? '');
  const s3Host = host === 'console.aws.amazon.com' ? 's3.console.aws.amazon.com' : host;
  const base = `https://${s3Host}/s3/buckets/${encodeURIComponent(bucket)}`;
  return prefix ? `${base}?prefix=${encodeURIComponent(prefix)}` : base;
}

// The CloudWatch console encodes path segments twice (each `/` becomes `$252F`): encodeURIComponent
// once, then turn the resulting `%` into `$25`.
function cwEncode(s: string): string {
  return encodeURIComponent(s).replace(/%/g, '$25');
}

/**
 * CloudWatch Logs console at a specific Batch log stream in `region`, in `logGroup` (default: the
 * fleet-wide `/aws/batch/job`).
 */
export function cloudwatchLogUrl(
  logStream: string | null | undefined,
  region: string | null | undefined,
  logGroup?: string | null,
): string | null {
  const rgn = validRegion(region);
  if (!logStream || !rgn) return null;
  const group = logGroup || BATCH_LOG_GROUP;
  return (
    `${regionalConsole(rgn)}/cloudwatch/home?region=${rgn}` +
    `#logsV2:log-groups/log-group/${cwEncode(group)}/log-events/${cwEncode(logStream)}`
  );
}

/**
 * AWS Batch console at a specific job's detail in `region`. The current console routes job detail
 * by compute type (`#jobs/<type>/detail/<id>`); the legacy `#jobs/detail/<id>` route resolves to a
 * "job cannot be found" page even for a live job. Every biffix queue-class (coordinator and
 * workers) is EC2, so the type is always `ec2`.
 */
export function batchJobUrl(
  jobId: string | null | undefined,
  region: string | null | undefined,
): string | null {
  const rgn = validRegion(region);
  if (!jobId || !rgn) return null;
  return `${regionalConsole(rgn)}/batch/home?region=${rgn}#jobs/ec2/detail/${encodeURIComponent(jobId)}`;
}

/**
 * Cognito console users list for a user pool. Lands on the pool's users, where the shown submitter
 * email can be searched. A per-user deep link is deliberately avoided: the run records the token
 * `sub`, but the Cognito console keys users by their username, which differs from the sub for a
 * federated (e.g. Google) user — so a sub-based deep link would not resolve. The region defaults to
 * the pool's own: a pool id is `<region>_<id>`. Returns null without a pool id or a region.
 */
export function cognitoUsersUrl(
  poolId: string | null | undefined,
  region?: string | null,
): string | null {
  if (!poolId) return null;
  const rgn = validRegion(region) ?? validRegion(poolId.split('_')[0]);
  if (!rgn) return null;
  return `${regionalConsole(rgn)}/cognito/v2/idp/user-pools/${encodeURIComponent(poolId)}/users?region=${rgn}`;
}

/**
 * ECR console at a specific image (by digest) in a private repository. Needs the registry/account
 * id, repository, region and the `sha256:...` digest — all captured on the frozen workflow version
 * when the declared image was an ECR reference. Returns null when any coordinate is missing (e.g. a
 * version registered from a bare digest).
 */
export function ecrImageUrl(
  registryId: string | null | undefined,
  repository: string | null | undefined,
  digest: string | null | undefined,
  region: string | null | undefined,
): string | null {
  const rgn = validRegion(region);
  if (!registryId || !repository || !digest || !rgn) return null;
  return (
    `${regionalConsole(rgn)}/ecr/repositories/private/${registryId}/` +
    `${encodeURIComponent(repository)}/_/image/${encodeURIComponent(digest)}/details?region=${rgn}`
  );
}
