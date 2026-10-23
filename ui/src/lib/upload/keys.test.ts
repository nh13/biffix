import { describe, expect, it } from 'vitest';
import { defaultUploadFolder, renamedKey, uploadKey } from './keys';

describe('upload destinations', () => {
  // Local time: just after midnight, which is still the previous day in UTC east of Greenwich.
  const day = new Date(2026, 8, 24, 0, 30);
  it('defaults to the browsed uploads folder, else a folder for the local day', () => {
    expect(defaultUploadFolder('uploads/batch-3/', day)).toBe('uploads/batch-3/');
    expect(defaultUploadFolder('', day)).toBe('uploads/2026-09-24/');
    expect(defaultUploadFolder('data/', day)).toBe('uploads/2026-09-24/');
  });
  it('keys a file under an uploads folder only', () => {
    expect(uploadKey('uploads/2026-09-24', 'A_R1.fq.gz')).toEqual({
      key: 'uploads/2026-09-24/A_R1.fq.gz',
    });
    expect(uploadKey('/uploads/x/', 'a')).toEqual({ key: 'uploads/x/a' });
    for (const bad of ['data/', 'uploads/../biffix/', 'uploads//x/', 'uploads/./'])
      expect(uploadKey(bad, 'a')).toHaveProperty('error');
  });
  it('says what is wrong with a file name', () => {
    expect(uploadKey('uploads/x/', '')).toEqual({
      error: 'A file with no name cannot be uploaded',
    });
    expect(uploadKey('uploads/x/', 'a\tb')).toEqual({
      error: 'The file name "a\\tb" has a control character',
    });
  });
  it('refuses what the backend refuses: any control character, or a key over 1024 bytes', () => {
    expect(uploadKey('uploads/x/', 'a\u0085b')).toHaveProperty('error');
    expect(uploadKey('uploads/x/', 'a\u007fb')).toHaveProperty('error');
    expect(uploadKey('uploads/a\u0001/', 'b')).toHaveProperty('error');
    // 1024 bytes in all is allowed; one more is not (a multi-byte character counts as its bytes).
    const fits = 'a'.repeat(1024 - 'uploads/x/'.length);
    expect(uploadKey('uploads/x/', fits)).toEqual({ key: `uploads/x/${fits}` });
    expect(uploadKey('uploads/x/', `${fits.slice(1)}é`)).toEqual({
      error: 'The file name and folder are too long (at most 1024 bytes)',
    });
  });
  it('offers another name before the extensions when a key is taken', () => {
    expect(renamedKey('uploads/x/P01_R1.fastq.gz')).toBe('uploads/x/P01_R1-2.fastq.gz');
    expect(renamedKey('uploads/x/genome.fa', 3)).toBe('uploads/x/genome-3.fa');
    expect(renamedKey('uploads/x/README')).toBe('uploads/x/README-2');
    expect(renamedKey('uploads/x/.env')).toBe('uploads/x/.env-2');
  });
});
