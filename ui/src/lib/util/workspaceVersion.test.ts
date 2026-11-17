import { describe, expect, it } from 'vitest';
import { workspaceVersion } from './workspaceVersion';

describe('workspaceVersion', () => {
  it("reads [workspace.package]'s version, not another table's", () => {
    const cargo = [
      '[workspace]',
      'members = ["crates/*"]',
      '',
      '[workspace.dependencies]',
      'version = "9.9.9"',
      '',
      '[workspace.package]',
      '# shared by every crate',
      '',
      'edition = "2024"',
      'version   =   "1.2.3-rc.1"',
      '',
      '[profile.release]',
      'version = "8.8.8"',
    ].join('\n');
    expect(workspaceVersion(cargo)).toBe('1.2.3-rc.1');
  });

  it('takes CRLF line endings', () => {
    expect(workspaceVersion('[workspace.package]\r\nversion = "0.2.0"\r\n')).toBe('0.2.0');
  });

  it('refuses a Cargo.toml with no workspace version', () => {
    expect(() => workspaceVersion('[package]\nversion = "0.2.0"\n')).toThrow(
      'no version in [workspace.package] of Cargo.toml',
    );
  });
});
