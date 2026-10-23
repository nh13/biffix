import { describe, expect, it } from 'vitest';
import type { SiteSettings } from '$lib/api';
import {
  MAX_FILE_KB,
  logoTooLarge,
  recipientStatus,
  siteChanges,
  supportChanged,
  unreadText,
} from './settings';

const stored = (s: Partial<SiteSettings> = {}): SiteSettings => s as SiteSettings;

describe('siteChanges', () => {
  it('sends nothing before the settings have loaded', () => {
    expect(siteChanges(null, 'Lab', 'data:x')).toEqual({});
  });

  it('sends only the fields that differ from what is stored', () => {
    const s = stored({ title: 'Lab', logo_data_url: 'data:a' });
    expect(siteChanges(s, 'Lab', 'data:a')).toEqual({});
    expect(siteChanges(s, ' New ', 'data:a')).toEqual({ title: 'New' });
    expect(siteChanges(s, 'Lab', null)).toEqual({ logo_data_url: null });
  });

  it('clears a title typed as blank', () => {
    expect(siteChanges(stored({ title: 'Lab' }), '  ', null)).toEqual({ title: null });
    expect(siteChanges(stored(), '', null)).toEqual({});
  });
});

describe('supportChanged', () => {
  const f = { name: 'Sam', email: 'sam@lab.example.org', url: '' };

  it('is false before the settings have loaded', () => {
    expect(supportChanged(null, f)).toBe(false);
  });

  it('compares the trimmed fields with the stored contact', () => {
    const s = stored({ support: { name: 'Sam', email: 'sam@lab.example.org', url: null } });
    expect(supportChanged(s, { ...f, name: ' Sam ' })).toBe(false);
    expect(supportChanged(s, { ...f, url: 'https://help' })).toBe(true);
    expect(supportChanged(stored(), f)).toBe(true);
    expect(supportChanged(stored(), { name: '', email: '', url: '' })).toBe(false);
  });
});

describe('logoTooLarge', () => {
  it('names the file’s size and the limit', () => {
    expect(MAX_FILE_KB).toBe(384);
    expect(logoTooLarge(600 * 1024)).toBe('Image too large (600 KB; the limit is about 384 KB).');
  });
});

describe('recipientStatus', () => {
  it('says the known statuses in words and an unknown one as itself', () => {
    expect(recipientStatus('pending')).toBe('awaiting confirmation');
    expect(recipientStatus('confirmed')).toBe('confirmed');
    expect(recipientStatus('bouncing')).toBe('bouncing');
  });
});

describe('unreadText', () => {
  it('says nothing until loaded, then the count', () => {
    expect(unreadText(null)).toBe('');
    expect(unreadText(0)).toBe('Nothing unread.');
    expect(unreadText(3)).toBe('3 unread.');
  });
});
