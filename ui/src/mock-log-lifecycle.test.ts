import { expect, it } from 'vitest';

it.each([
  { reason: 'spot reclaim', interruption: 'spot_reclaimed', exitCode: 0 },
  { reason: 'host loss', interruption: 'host_lost', exitCode: 0 },
  { reason: 'unknown interruption', interruption: 'future_host_loss', exitCode: 0 },
  { reason: 'missing exit code', interruption: undefined, exitCode: undefined },
  { reason: 'unsuccessful exit', interruption: undefined, exitCode: 1 },
  { reason: 'missing stream', interruption: undefined, exitCode: 0, missingStream: true },
])(
  'keeps coordinator logs open after engine success and $reason until replacement finishes',
  async ({ interruption, exitCode, missingStream }) => {
    const { RUN_ROUTES } = await import(new URL('../mock/handlers/runs.mjs', import.meta.url).href);
    const { RUNS } = await import(new URL('../mock/fixtures/runs.mjs', import.meta.url).href);
    const handler = RUN_ROUTES.find(
      ([method, path]: string[]) => method === 'GET' && path === '/runs/{run_id}/coordinator-logs',
    )[2];
    const old: { log_stream?: string; ended: boolean; interruption?: string; exit_code?: number } =
      {
        log_stream: missingStream ? undefined : 'old',
        ended: true,
        interruption,
        exit_code: exitCode,
      };
    const run = {
      run_id: 'coordinator-retry-regression',
      state: 'succeeded',
      coordinator_job_id: 'job',
      coordinator_executions: [old],
    };
    const read = () => {
      let response: unknown;
      handler({
        params: { run_id: run.run_id },
        query: new URLSearchParams(),
        send: (status: number, page: unknown) => {
          expect(status).toBe(200);
          response = page;
        },
      });
      return response;
    };
    RUNS.push(run);
    try {
      expect(read()).toMatchObject({
        log_stream: missingStream ? 'mock/coordinator/coordinator-retry-regression' : 'old',
        ended: false,
      });
      const replacement = {
        log_stream: 'cleanup',
        ended: false,
        interruption: undefined,
        exit_code: 0,
      };
      run.coordinator_executions = [old, replacement];
      expect(read()).toMatchObject({ log_stream: 'cleanup', ended: false });
      replacement.ended = true;
      expect(read()).toMatchObject({ log_stream: 'cleanup', ended: true });
    } finally {
      RUNS.splice(RUNS.indexOf(run), 1);
    }
  },
);
