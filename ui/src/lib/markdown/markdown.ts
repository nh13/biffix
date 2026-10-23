// A workflow's About page: its author's Markdown rendered as safe HTML. The Markdown comes from the
// workflow repo, so it is untrusted — every render goes through DOMPurify.

import DOMPurify from 'dompurify';
import { Marked, type Token } from 'marked';

const marked = new Marked({ gfm: true, async: false });

/** Links and images that point somewhere on their own; relative ones point into a repo we can't serve. */
const ABSOLUTE = /^(https?:|mailto:)/i;

/**
 * Attributes that make the browser fetch or navigate to a URL. Only `a`/`area` links and `img`
 * sources are kept (under the rules below); on any other element they are dropped, so nothing else
 * the sanitizer lets through (an SVG `image`/`use`, say) fetches a third-party URL.
 */
const URL_ATTRS = ['href', 'src', 'xlink:href'];

/** What a parameter's help may hold: inline formatting, lists, code and links. A heading would break
 * the form's heading outline, and a table or an image does not belong in a field's ⓘ (an image
 * would be fetched from wherever the author pointed it); their text is kept, the element dropped. */
const HELP_TAGS = [
  'p',
  'br',
  'strong',
  'b',
  'em',
  'i',
  'del',
  's',
  'code',
  'pre',
  'ul',
  'ol',
  'li',
  'a',
];

function sanitize(html: string, allowedTags?: string[]): string {
  const fragment = DOMPurify.sanitize(html, {
    RETURN_DOM_FRAGMENT: true,
    ...(allowedTags ? { ALLOWED_TAGS: allowedTags, KEEP_CONTENT: true } : {}),
    // Media elements fetch their sources with no way to withhold the Referer, so they go; so do
    // form controls (a GFM task list's checkbox included: its item keeps its text).
    FORBID_TAGS: [
      'style',
      'form',
      'input',
      'button',
      'textarea',
      'select',
      'video',
      'audio',
      'source',
      'track',
    ],
    // More URL-bearing attributes: srcset/poster/background fetch, ping posts on click. `class` and
    // `id` would let a page reuse the app's own styles (a fake badge or error) and ids.
    FORBID_ATTR: ['style', 'srcset', 'poster', 'background', 'ping', 'class', 'id'],
  });
  for (const a of fragment.querySelectorAll('a, area')) {
    const href = a.getAttribute('href') ?? '';
    if (ABSOLUTE.test(href)) {
      a.setAttribute('target', '_blank');
      a.setAttribute('rel', 'noopener noreferrer');
    } else {
      // Relative links lead nowhere here, and no heading carries an id for a `#fragment` one.
      a.replaceWith(...a.childNodes);
    }
  }
  for (const el of fragment.querySelectorAll('*')) {
    if (el.localName === 'a' || el.localName === 'area' || el.localName === 'img') continue;
    for (const name of el.getAttributeNames()) {
      if (URL_ATTRS.includes(name.toLowerCase())) el.removeAttribute(name);
    }
  }
  for (const img of fragment.querySelectorAll('img')) {
    if (ABSOLUTE.test(img.getAttribute('src') ?? '')) {
      img.setAttribute('loading', 'lazy');
      img.setAttribute('referrerpolicy', 'no-referrer');
    } else {
      img.replaceWith(document.createTextNode(img.getAttribute('alt') ?? ''));
    }
  }
  const holder = document.createElement('div');
  holder.append(fragment);
  return holder.innerHTML;
}

/** The whole About page as safe HTML. */
export function renderMarkdown(markdown: string): string {
  return sanitize(marked.parse(markdown) as string);
}

/** A parameter's help as safe HTML, limited to [`HELP_TAGS`]. */
export function renderHelpMarkdown(markdown: string): string {
  return sanitize(marked.parse(markdown) as string, HELP_TAGS);
}

/**
 * The first prose paragraph of an About page as safe HTML (headings, badges and code skipped), for
 * the Run tab's teaser; `null` when there is none.
 */
export function firstParagraph(markdown: string): string | null {
  for (const token of marked.lexer(markdown)) {
    // A paragraph of only badges/images has no text worth showing. Judged on the tokens: the
    // sanitizer turns a relative image into its alt text, which is not prose.
    if (token.type === 'paragraph' && hasProse(token.tokens ?? []))
      return sanitize(marked.parser([token])).trim();
  }
  return null;
}

/** Whether inline `tokens` hold text besides images (and links made only of images). */
function hasProse(tokens: readonly Token[]): boolean {
  return tokens.some((t) => {
    if (t.type === 'image') return false;
    if (t.type === 'text' || t.type === 'escape') return t.text.trim() !== '';
    if ('tokens' in t && t.tokens) return hasProse(t.tokens);
    return t.type !== 'br' && t.type !== 'space';
  });
}
