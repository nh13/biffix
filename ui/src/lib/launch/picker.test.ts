import { describe, expect, it } from 'vitest';
import { isS3Uri, locate, matchesSuffix, parentFolder } from './picker';
import type { DataSource } from '$lib/api';

const src = (id: string, uri: string): DataSource => ({
  id,
  name: id,
  kind: 'data',
  uri,
  writable: false,
});

describe('picker helpers', () => {
  it('filters by suffix, case-insensitively', () => {
    expect(matchesSuffix('P1_R1.FASTQ.GZ', ['.fastq.gz', '.fq.gz'])).toBe(true);
    expect(matchesSuffix('notes.txt', ['.fastq.gz'])).toBe(false);
    expect(matchesSuffix('anything', [])).toBe(true);
  });
  it('finds the parent folder', () => {
    expect(parentFolder('a/b/c.txt')).toBe('a/b/');
    expect(parentFolder('c.txt')).toBe('');
    expect(parentFolder('a/b/')).toBe('a/');
  });
  it('locates a URI in the most specific source', () => {
    const sources = [src('project', 's3://b/'), src('core', 's3://b/data/')];
    expect(locate(sources, 's3://b/data/wgs/x.fq.gz')).toMatchObject({
      source: { id: 'core' },
      folder: 'wgs/',
    });
    expect(locate(sources, 's3://b/uploads/')).toMatchObject({
      source: { id: 'project' },
      folder: 'uploads/',
    });
    expect(locate(sources, 's3://other/x')).toBeNull();
  });
  it('recognizes S3 URIs', () => {
    expect(isS3Uri('s3://bucket/key.txt')).toBe(true);
    expect(isS3Uri(' s3://bucket/ ')).toBe(true);
    expect(isS3Uri('bucket/key')).toBe(false);
    expect(isS3Uri('s3://bucket')).toBe(false);
  });
});
