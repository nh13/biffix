// A page that could not be shown, in plain words: what kind of failure it was (not found, no
// access, the server's fault, no connection), said in the view's vocabulary with a way back. The
// technical line stays available behind *Details*. Pure, so it is unit-tested.

import { ApiError, AuthRequiredError } from '#lib/api/index.js';
import type { ViewMode } from '#lib/view/viewRoutes.js';
import { t } from '#lib/view/vocab.js';

export type ErrorKind =
  'not-found' | 'bad-request' | 'forbidden' | 'signed-out' | 'server' | 'network';

/** What could not be shown. */
export type ErrorSubject = 'run' | 'workflow' | 'experiment' | 'data' | 'page';

/**
 * The kind of a failure: from its HTTP status when it has one; a failed `fetch` (a `TypeError`:
 * nothing answered) is no connection, and a sign-in that is needed is its own kind. Anything else
 * (an answer that could not be read, a bug) is the server's side, never "no connection".
 */
export function errorKind(error: unknown): ErrorKind {
  if (error instanceof AuthRequiredError) return 'signed-out';
  const status =
    error instanceof ApiError ? error.status : typeof error === 'number' ? error : undefined;
  if (status === undefined) return error instanceof TypeError ? 'network' : 'server';
  if (status === 404 || status === 410) return 'not-found';
  if (status === 400) return 'bad-request';
  if (status === 401) return 'signed-out';
  if (status === 403) return 'forbidden';
  return 'server';
}

/** What could not be shown, in `mode`'s words (the view's vocabulary where it has them). */
function nounOf(subject: ErrorSubject, mode: ViewMode): string {
  switch (subject) {
    case 'run':
      return t('runLower', mode);
    case 'workflow':
      return t('workflowLower', mode);
    case 'data':
      return mode === 'lab' ? 'folder' : 'data source';
    default:
      return subject;
  }
}

/** Where "back" goes for each subject, and the place the link names. */
function backOf(subject: ErrorSubject, mode: ViewMode): { href: string; label: string } {
  const [href, place] =
    subject === 'run'
      ? ['/runs', t('runs', mode)]
      : subject === 'workflow'
        ? ['/workflows', t('workflows', mode)]
        : subject === 'experiment'
          ? ['/experiments', 'Experiments']
          : subject === 'data'
            ? ['/data', 'Data']
            : ['/', t('home', mode)];
  return { href, label: `Back to ${place}` };
}

export interface ErrorWords {
  title: string;
  body: string;
  back: { href: string; label: string };
  /** Whether trying again may help (the server's fault, or no connection). */
  retry: boolean;
  /** Whether the page offers *Sign in* (the session ended). */
  signIn: boolean;
}

/** The words for a `kind` failure to show `subject`, in `mode`'s vocabulary. */
export function errorWords(kind: ErrorKind, subject: ErrorSubject, mode: ViewMode): ErrorWords {
  const noun = nounOf(subject, mode);
  const back = backOf(subject, mode);
  const a = /^[aeiou]/.test(noun) ? 'an' : 'a';
  switch (kind) {
    case 'not-found':
      return {
        title: `This ${noun} doesn't exist`,
        body:
          subject === 'page'
            ? 'The link may be mistyped, or the page may have moved.'
            : `The link may be mistyped, or the ${noun} may have been removed.`,
        back,
        retry: false,
        signIn: false,
      };
    case 'bad-request':
      return {
        title: `This ${noun} couldn't be opened`,
        body: `biffix could not read the link to it. Check the link, or open ${a} ${noun} from the list.`,
        back,
        retry: false,
        signIn: false,
      };
    case 'forbidden':
      // Access is by role (Viewer, Operator, Developer, Admin), not project membership.
      return {
        title: `You don't have access to this ${noun}`,
        body: `Your account isn't allowed to see this ${noun}. Ask whoever runs biffix for your lab for access.`,
        back,
        retry: false,
        signIn: false,
      };
    case 'signed-out':
      return {
        title: 'Sign in again',
        // Never "taking you there": the redirect-loop guard may have held the sign-in back.
        body: 'Your session has ended. Sign in again to carry on.',
        back,
        retry: false,
        signIn: true,
      };
    case 'server':
      return {
        title: `biffix couldn't load this ${noun}`,
        body: 'Something went wrong on our side. Try again in a minute; if it keeps happening, tell whoever runs biffix for your lab.',
        back,
        retry: true,
        signIn: false,
      };
    case 'network':
      return {
        title: "biffix can't be reached",
        body: 'Check your connection (or VPN), then try again.',
        back,
        retry: true,
        signIn: false,
      };
  }
}
