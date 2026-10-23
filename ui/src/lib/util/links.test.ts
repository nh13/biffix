import { describe, expect, it } from 'vitest';

import { batchJobUrl, cloudwatchLogUrl, cognitoUsersUrl, ecrImageUrl, s3ConsoleUrl } from './links';

describe('the partition', () => {
  it('links GovCloud and China to their own consoles, never the commercial one', () => {
    expect(batchJobUrl('j', 'us-gov-west-1')).toMatch(
      /^https:\/\/us-gov-west-1\.console\.amazonaws-us-gov\.com\/batch\//,
    );
    expect(cloudwatchLogUrl('s', 'cn-north-1')).toMatch(
      /^https:\/\/cn-north-1\.console\.amazonaws\.cn\/cloudwatch\//,
    );
    expect(cognitoUsersUrl('us-gov-west-1_abc')).toMatch(/console\.amazonaws-us-gov\.com/);
    expect(ecrImageUrl('1', 'r', 'sha256:a', 'cn-north-1')).toMatch(/console\.amazonaws\.cn/);
    expect(s3ConsoleUrl('s3://b/p', 'us-gov-west-1')).toBe(
      'https://console.amazonaws-us-gov.com/s3/buckets/b?prefix=p',
    );
    expect(s3ConsoleUrl('s3://b/p', 'eu-west-2')).toBe(
      'https://s3.console.aws.amazon.com/s3/buckets/b?prefix=p',
    );
  });
});

describe('s3ConsoleUrl', () => {
  it('links to the bucket, adding the prefix when present, and leaves the region to the console', () => {
    expect(s3ConsoleUrl('s3://my-bucket/some/prefix')).toBe(
      'https://s3.console.aws.amazon.com/s3/buckets/my-bucket?prefix=some%2Fprefix',
    );
    expect(s3ConsoleUrl('s3://my-bucket')).toBe(
      'https://s3.console.aws.amazon.com/s3/buckets/my-bucket',
    );
    expect(s3ConsoleUrl('s3://my-bucket/')).toBe(
      'https://s3.console.aws.amazon.com/s3/buckets/my-bucket',
    );
  });
  it('returns null for a missing or non-s3 uri', () => {
    expect(s3ConsoleUrl(null)).toBeNull();
    expect(s3ConsoleUrl(undefined)).toBeNull();
    expect(s3ConsoleUrl('https://example.com/x')).toBeNull();
  });
});

describe('cloudwatchLogUrl', () => {
  it('double-encodes path segments as $252F (the CloudWatch console scheme), in the given region', () => {
    // The log-group must be the twice-encoded `/aws/batch/job`; the stream's `/` and space likewise.
    expect(cloudwatchLogUrl('job/stream name', 'eu-west-1')).toBe(
      'https://eu-west-1.console.aws.amazon.com/cloudwatch/home?region=eu-west-1' +
        '#logsV2:log-groups/log-group/$252Faws$252Fbatch$252Fjob/log-events/job$252Fstream$2520name',
    );
  });
  it('opens the stream in the log group given, else /aws/batch/job', () => {
    expect(cloudwatchLogUrl('s', 'us-east-1', '/biffix/project-a/batch')).toBe(
      'https://us-east-1.console.aws.amazon.com/cloudwatch/home?region=us-east-1' +
        '#logsV2:log-groups/log-group/$252Fbiffix$252Fproject-a$252Fbatch/log-events/s',
    );
    expect(cloudwatchLogUrl('s', 'us-east-1', null)).toBe(cloudwatchLogUrl('s', 'us-east-1'));
  });
  it('returns null without a stream or a region', () => {
    expect(cloudwatchLogUrl(null, 'us-east-1')).toBeNull();
    expect(cloudwatchLogUrl(undefined, 'us-east-1')).toBeNull();
    expect(cloudwatchLogUrl('s', null)).toBeNull();
    expect(cloudwatchLogUrl('s', 'not a region')).toBeNull();
  });
});

describe('batchJobUrl', () => {
  it('links to a job detail in the given region, encoding the id', () => {
    expect(batchJobUrl('job-123', 'us-west-2')).toBe(
      'https://us-west-2.console.aws.amazon.com/batch/home?region=us-west-2#jobs/ec2/detail/job-123',
    );
  });
  it('returns null without a job id or a region', () => {
    expect(batchJobUrl(null, 'us-west-2')).toBeNull();
    expect(batchJobUrl('job-123', undefined)).toBeNull();
    // Never a region that could redirect the host part elsewhere.
    expect(batchJobUrl('job-123', 'evil.example/x')).toBeNull();
  });
});

describe('cognitoUsersUrl', () => {
  it('takes the region from the pool id, or an explicit one', () => {
    expect(cognitoUsersUrl('us-west-2_ABC')).toBe(
      'https://us-west-2.console.aws.amazon.com/cognito/v2/idp/user-pools/us-west-2_ABC/users?region=us-west-2',
    );
    expect(cognitoUsersUrl('us-east-1_ABC', 'eu-west-1')).toBe(
      'https://eu-west-1.console.aws.amazon.com/cognito/v2/idp/user-pools/us-east-1_ABC/users?region=eu-west-1',
    );
  });
  it('returns null without a pool id, or with no region to be had', () => {
    expect(cognitoUsersUrl(null)).toBeNull();
    expect(cognitoUsersUrl(undefined)).toBeNull();
    expect(cognitoUsersUrl('mock-pool')).toBeNull();
  });
});

describe('ecrImageUrl', () => {
  it('links to an image by digest when every coordinate is present', () => {
    expect(ecrImageUrl('123456789012', 'biffix/rnaseq', 'sha256:abc', 'ap-southeast-2')).toBe(
      'https://ap-southeast-2.console.aws.amazon.com/ecr/repositories/private/123456789012/' +
        'biffix%2Frnaseq/_/image/sha256%3Aabc/details?region=ap-southeast-2',
    );
  });
  it('returns null when any coordinate is missing', () => {
    expect(ecrImageUrl(null, 'repo', 'sha256:abc', 'us-east-1')).toBeNull();
    expect(ecrImageUrl('123', null, 'sha256:abc', 'us-east-1')).toBeNull();
    expect(ecrImageUrl('123', 'repo', null, 'us-east-1')).toBeNull();
    expect(ecrImageUrl('123', 'repo', 'sha256:abc', null)).toBeNull();
  });
});
