// *Copy report for support* and *Ask <name>* on a failed analysis: a plain-text
// bundle of what the page already shows (about 1 KB, never a secret), and the email that asks for
// help. Built in the browser from data already fetched. Pure, so it is unit-tested.

import type { FailureSummary, RunDetail } from '#lib/api/index.js';
import { REDACTED_SECRET_PLACEHOLDER } from '#lib/launch/form.js';
import { failureLine } from './failure';
import { runName } from '#lib/runs/runName.js';
import { paramEntries } from '#lib/runs/runFacts.js';

type SupportContact = NonNullable<FailureSummary['support']>;

/** The run facts the report reads. */
export type ReportRun = Pick<
  RunDetail,
  | 'run_id'
  | 'run_name'
  | 'workflow'
  | 'workflow_name'
  | 'workflow_title'
  | 'submitter'
  | 'submitter_label'
  | 'submitted_at'
  | 'completed_at'
  | 'state'
  | 'params'
  | 'input_uris'
  | 'reference_bundle'
  | 'samples'
>;

export interface SupportReportInput {
  run: ReportRun;
  /** The analysis page's address. */
  url: string;
  /** The failures, as the server explained them (none for a cancelled run). */
  items: FailureSummary['items'];
  /** Why the failures could not be read (or that they are still loading), when they could not. */
  failuresUnread?: string | null;
  /** The version's form fields: a field marked `is_secret` is never written, whatever it holds. */
  fields?: readonly { name: string; is_secret?: boolean }[];
}

/** The most log lines the report carries per failure. */
export const REPORT_LOG_LINES = 20;
/** What a secret param is written as. */
export const SECRET_TEXT = '[secret]';

const when = (secs: number | null | undefined) =>
  secs == null ? 'not yet' : new Date(secs * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');

const text = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v));

/**
 * The support report: the analysis and where it is, its pipeline, who started it and when, its
 * state and sample counts, each failure (sample, step, task, cause, message, tries, exit code,
 * Batch job, what AWS said, the end of its log), the reference bundle, the params (secret ones as
 * `[secret]`) and the inputs. (The UI does not know the biffix release, so the report leaves it
 * out.)
 */
export function supportReport({
  run,
  url,
  items,
  failuresUnread = null,
  fields = [],
}: SupportReportInput): string {
  const secret = new Set(fields.filter((f) => f.is_secret).map((f) => f.name));
  const lines = [
    'biffix support report',
    `Analysis: ${runName(run)}`,
    `Link: ${url}`,
    `Run id: ${run.run_id}`,
    `Pipeline: ${run.workflow_name ?? 'unknown'} (version ${run.workflow ?? 'unknown'})`,
    `Started by: ${run.submitter_label ?? run.submitter ?? 'unknown'}`,
    `Submitted: ${when(run.submitted_at)}`,
    `Finished: ${when(run.completed_at)}`,
    `State: ${run.state}`,
  ];
  const s = run.samples;
  if (s && s.total > 0)
    lines.push(
      `Samples: ${s.total} ${s.declared ? 'total' : 'seen'}, ${s.done} done, ${s.failed} failed`,
    );
  if (items.length === 0 && failuresUnread)
    lines.push('', `Failures: could not be read (${failuresUnread})`);
  items.forEach((item, i) => {
    lines.push('', `Failure ${i + 1}: ${failureLine(item)}`);
    if (item.sample) lines.push(`  Sample: ${item.sample}`);
    lines.push(`  Step: ${item.step}`);
    if (item.task_key) lines.push(`  Task: ${item.task_key}`);
    lines.push(`  Cause: ${item.cause}`, `  Tries: ${item.attempts}`);
    if (item.exit_code != null) lines.push(`  Exit code: ${item.exit_code}`);
    if (item.batch_job_id) lines.push(`  Batch job: ${item.batch_job_id}`);
    if (item.stage) lines.push(`  Stopped at: ${item.stage}`);
    if (item.reason) lines.push(`  Reason: ${item.reason}`);
    const tail = item.log_tail.slice(-REPORT_LOG_LINES);
    if (tail.length) lines.push(`  Last ${tail.length} log lines:`, ...tail.map((l) => `    ${l}`));
  });
  lines.push('', `Reference bundle: ${run.reference_bundle?.id ?? 'none'}`);
  const params = paramEntries(run.params);
  lines.push(params.length ? 'Params:' : 'Params: none');
  for (const [name, value] of params) {
    // The server redacts secrets already; a field this version marks secret is kept out regardless.
    const hidden = secret.has(name) || value === REDACTED_SECRET_PLACEHOLDER;
    lines.push(`  ${name}: ${hidden ? SECRET_TEXT : text(value)}`);
  }
  const inputs = run.input_uris ?? [];
  lines.push(inputs.length ? 'Inputs:' : 'Inputs: none', ...inputs.map((u) => `  ${u}`));
  return `${lines.join('\n')}\n`;
}

/**
 * `email` as a `mailto:` URL's address: percent-encoded but for its `@`, so a character the address
 * may legally hold (`?`, `&`, `#`, `%`, `,`) cannot add a header, a recipient or a fragment.
 */
export function mailtoAddress(email: string): string {
  return encodeURIComponent(email.trim()).replaceAll('%40', '@');
}

/**
 * The *Ask <name>* email: to the support contact, subject *biffix: <analysis> needs attention*, the
 * body the analysis's link, the one-line diagnosis, and where the copied report goes.
 */
export function askHref(
  contact: SupportContact,
  analysis: string,
  url: string,
  diagnosis: string | null,
): string {
  const subject = `biffix: ${analysis} needs attention`;
  const body = [
    `Hi ${contact.name},`,
    '',
    `My analysis "${analysis}" needs attention: ${url}`,
    ...(diagnosis ? ['', `biffix says: ${diagnosis}`] : []),
    '',
    'Paste the copied report here.',
  ].join('\n');
  return `mailto:${mailtoAddress(contact.email)}?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`;
}
