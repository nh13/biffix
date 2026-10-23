// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { firstParagraph, renderHelpMarkdown, renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  it('renders GFM', () => {
    const html = renderMarkdown('# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n`code`');
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toContain('<table>');
    expect(html).toContain('<code>code</code>');
  });

  it('strips scripts, handlers and javascript: links', () => {
    const html = renderMarkdown(
      '<script>alert(1)</script>\n\n<img src="https://x/a.png" onerror="alert(2)">\n\n' +
        '[click](javascript:alert(3)) <a href="https://x" onclick="alert(4)">x</a>\n\n' +
        '<p style="position:fixed">styled</p><form><input></form>',
    );
    expect(html).not.toMatch(/script|onerror|onclick|javascript:|style=|<form|<input/i);
    expect(html).toContain('styled');
  });

  it('opens absolute links safely and leaves relative ones as text', () => {
    const html = renderMarkdown(
      '[docs](https://nf-co.re) [local](docs/usage.md) [top](#usage) ![diagram](img/flow.png)',
    );
    expect(html).toContain(
      '<a href="https://nf-co.re" target="_blank" rel="noopener noreferrer">docs</a>',
    );
    expect(html).not.toContain('docs/usage.md');
    expect(html).toContain('local');
    // No heading carries an id, so a #fragment link leads nowhere: its text stays.
    expect(html).not.toContain('href="#usage"');
    expect(html).toContain('top');
    expect(html).not.toContain('<img');
    expect(html).toContain('diagram');
  });

  it('keeps an absolute image, lazy and without a referrer', () => {
    const holder = document.createElement('div');
    holder.innerHTML = renderMarkdown('![logo](https://x/a.png) ![diagram](img/flow.png)');
    const imgs = holder.querySelectorAll('img');
    expect(imgs).toHaveLength(1);
    expect(imgs[0].getAttribute('src')).toBe('https://x/a.png');
    expect(imgs[0].getAttribute('alt')).toBe('logo');
    expect(imgs[0].getAttribute('loading')).toBe('lazy');
    expect(imgs[0].getAttribute('referrerpolicy')).toBe('no-referrer');
  });

  it('applies the URL policy to every URL-bearing element, not just links and images', () => {
    const holder = document.createElement('div');
    holder.innerHTML = renderMarkdown(
      '<video src="https://x/v.mp4" poster="https://x/p.png">fallback<source src="https://x/s.mp4"></video>\n\n' +
        '<audio src="https://x/a.mp3"></audio>\n\n' +
        '<img src="https://x/a.png" srcset="https://x/a2.png 2x, rel.png 1x">\n\n' +
        '<map name="m"><area href="docs/usage.md"><area href="https://x/y"></map>\n\n' +
        '<svg><image href="https://x/i.png"></image><use xlink:href="https://x/u.svg#a"></use></svg>\n\n' +
        '<a href="https://x/z" ping="https://x/ping">z</a>',
    );
    expect(holder.querySelector('video, audio, source, track')).toBeNull();
    expect(holder.querySelector('[srcset], [poster], [ping]')).toBeNull();
    expect(holder.querySelector('img')?.getAttribute('referrerpolicy')).toBe('no-referrer');
    const areas = holder.querySelectorAll('area');
    expect(areas).toHaveLength(1);
    expect(areas[0].getAttribute('href')).toBe('https://x/y');
    expect(areas[0].getAttribute('rel')).toBe('noopener noreferrer');
    // The SVG survived, so the loop below checks something.
    expect(holder.querySelector('svg image')).not.toBeNull();
    for (const el of holder.querySelectorAll('svg *')) {
      expect(el.getAttributeNames().filter((n) => /href|src/i.test(n))).toEqual([]);
    }
  });
});

describe('what an author cannot borrow from the app', () => {
  it("drops class and id, so Markdown can't dress as the app's own badges or errors", () => {
    const html = renderMarkdown('<span class="badge succeeded" id="main">ok</span>');
    expect(html).not.toContain('class=');
    expect(html).not.toContain('id=');
  });

  it('unwraps a #fragment link, which no heading carries the id for', () => {
    expect(renderMarkdown('[Usage](#usage)')).toBe('<p>Usage</p>\n');
  });
});

describe('firstParagraph', () => {
  it('skips headings and badge-only paragraphs', () => {
    const md = '# rnaseq\n\n[![CI](https://x/b.svg)](https://x)\n\nAligns **reads**.\n\nMore.';
    expect(firstParagraph(md)).toBe('<p>Aligns <strong>reads</strong>.</p>');
  });

  it('skips a badge or logo whose image is relative (its alt text is not prose)', () => {
    const md = '[![CI](img/ci.svg)](https://x)\n\n![logo](docs/logo.png)\n\nAligns reads.';
    expect(firstParagraph(md)).toBe('<p>Aligns reads.</p>');
  });

  it('is null without prose', () => {
    expect(firstParagraph('# Only a title\n\n```\ncode\n```')).toBeNull();
  });
});

describe('renderHelpMarkdown', () => {
  it('keeps inline formatting, lists, code and links, and drops headings, tables and images', () => {
    const html = renderHelpMarkdown(
      '# Aligner\n\n**STAR** or `hisat2`, see [docs](https://x/d).\n\n- one\n- two\n\n' +
        '| a |\n|---|\n| 1 |\n\n![logo](https://x/logo.png)\n\n<script>alert(1)</script>',
    );
    expect(html).toContain('<strong>STAR</strong>');
    expect(html).toContain('<code>hisat2</code>');
    expect(html).toContain('<li>one</li>');
    expect(html).toMatch(/<a href="https:\/\/x\/d"[^>]*rel="noopener noreferrer"/);
    expect(html).not.toMatch(/<h1|<table|<img|<script/);
    // A heading's text stays, as plain text.
    expect(html).toContain('Aligner');
  });
});
