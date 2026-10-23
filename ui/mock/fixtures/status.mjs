// The status probe's report: the overall status is the worst component's (`HealthStatus::worse`).
const STATUS_COMPONENTS = [
  { name: 'api', status: 'ok' },
  { name: 'database', status: 'ok' },
  { name: 'event_ingest', status: 'degraded', detail: 'ingest lag ~40s behind the event log' },
  { name: 'batch_compute', status: 'ok' },
  { name: 'reconciler', status: 'ok' },
  { name: 'alarms', status: 'ok' },
];
const SEVERITY = ['ok', 'degraded', 'down'];
export const STATUS = {
  status: SEVERITY[Math.max(...STATUS_COMPONENTS.map((c) => SEVERITY.indexOf(c.status)))],
  components: STATUS_COMPONENTS,
};
