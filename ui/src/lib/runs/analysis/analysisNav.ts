// Previous / next analysis on the analysis page: walk the list the user came from without
// going back to it. The Analyses list remembers the rows it showed (in this tab's sessionStorage,
// never the server: it is one viewer's way through the list); an analysis opened from anywhere else
// walks the user's own analyses by start time. Pure but for the storage, which is injectable, so it
// is unit-tested.

/** One analysis in the list walked: its id and the name the list showed for it. */
export interface NavItem {
  id: string;
  name: string;
}

/** The list the user came from: its rows in order, and the list's own URL (path and query). */
export interface NavList {
  items: NavItem[];
  href: string;
  /** The list has more pages than the one remembered: the walk ends at this page's edge. */
  more?: boolean;
}

const KEY = 'biffix-analysis-nav';

/** Remember the rows the Analyses list shows, for the analysis page's previous / next. */
export function saveNavList(list: NavList, storage: Storage | null = session()): void {
  try {
    storage?.setItem(KEY, JSON.stringify(list));
  } catch {
    // Not remembered: the page falls back to the user's analyses.
  }
}

/** The list remembered by [`saveNavList`], if any (and well formed). */
export function loadNavList(storage: Storage | null = session()): NavList | null {
  try {
    const raw = storage?.getItem(KEY);
    if (!raw) return null;
    const v = JSON.parse(raw) as Partial<NavList>;
    if (!Array.isArray(v.items) || typeof v.href !== 'string') return null;
    // "← Analyses" links to it: only the Analyses list itself, never another place (or site).
    if (!/^\/runs(?:[?#]|$)/.test(v.href)) return null;
    const items = v.items.filter(
      (i): i is NavItem => !!i && typeof i.id === 'string' && typeof i.name === 'string',
    );
    return { items, href: v.href, more: v.more === true };
  } catch {
    return null;
  }
}

/** The analyses before and after `id` in `items` (`null` at either end, or when it is not
 * listed). */
export function neighbours(
  items: readonly NavItem[],
  id: string,
): { prev: NavItem | null; next: NavItem | null; index: number } {
  const index = items.findIndex((i) => i.id === id);
  if (index < 0) return { prev: null, next: null, index };
  return {
    prev: index > 0 ? items[index - 1] : null,
    next: index < items.length - 1 ? items[index + 1] : null,
    index,
  };
}

/** What is open over the page that a previous / next shortcut must not navigate from under: a
 * dialog (modal or not, the tour's included), a modal sheet, or the narrow-screen nav drawer. */
export const OVERLAY_SELECTOR =
  'dialog[open], [role="dialog"], [role="alertdialog"], [aria-modal="true"], .drawer-open';

/** A key press, as the page shortcuts read it. */
export interface ShortcutKey {
  key: string;
  target: EventTarget | null;
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  defaultPrevented?: boolean;
  repeat?: boolean;
  isComposing?: boolean;
}

/**
 * Whether a page's own key shortcut must leave `e` alone: a modifier is held, it was handled
 * already (`defaultPrevented`), it is part of an IME composition, it is typed into a field, or it
 * is pressed behind an open dialog, drawer or tour (`overlayOpen`, or a target inside one).
 */
export function shortcutBlocked(e: ShortcutKey, overlayOpen = false): boolean {
  if (e.altKey || e.ctrlKey || e.metaKey) return true;
  if (e.defaultPrevented || e.isComposing || overlayOpen) return true;
  const el = e.target as {
    tagName?: string;
    isContentEditable?: boolean;
    closest?: (selector: string) => unknown;
  } | null;
  const tag = el?.tagName?.toLowerCase();
  if (tag === 'input' || tag === 'textarea' || tag === 'select' || el?.isContentEditable)
    return true;
  return typeof el?.closest === 'function' && !!el.closest(OVERLAY_SELECTOR);
}

/** Whether a key press is a previous / next shortcut (`[` / `]`): one `shortcutBlocked` lets
 * through, and not a held key repeating (which would walk the whole list). */
export function navKey(e: ShortcutKey, overlayOpen = false): 'prev' | 'next' | null {
  if (e.repeat || shortcutBlocked(e, overlayOpen)) return null;
  if (e.key === '[') return 'prev';
  if (e.key === ']') return 'next';
  return null;
}

/** Whether anything in `doc` is open over the page ([`OVERLAY_SELECTOR`]); focus can sit on the
 * page's body while a modal is up, so the target alone does not tell. */
export function overlayOpen(
  doc: Pick<Document, 'querySelector'> | null = globalDocument(),
): boolean {
  return !!doc?.querySelector(OVERLAY_SELECTOR);
}

/** Give a remembered row the name it has now (after a rename), so previous / next say it. */
export function renameInNavList(
  id: string,
  name: string,
  storage: Storage | null = session(),
): void {
  const list = loadNavList(storage);
  if (!list || !list.items.some((i) => i.id === id)) return;
  saveNavList(
    { ...list, items: list.items.map((i) => (i.id === id ? { ...i, name } : i)) },
    storage,
  );
}

function globalDocument(): Document | null {
  return typeof document === 'undefined' ? null : document;
}

function session(): Storage | null {
  try {
    return typeof sessionStorage === 'undefined' ? null : sessionStorage;
  } catch {
    return null;
  }
}
