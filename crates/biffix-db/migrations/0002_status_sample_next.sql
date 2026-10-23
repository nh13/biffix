-- How soon each status sample promises the next one. The reconcile tick skips its work while the
-- tenant is idle and runs in full once an hour, so the 5-minute heartbeat holds only while a
-- coordinator is active. The probe stamps each sample with the most seconds until the next full
-- tick (`biffix_db::idle::Why::next_full_within_secs`), and the status page calls a sample stale
-- only once that has passed (plus a grace): an idle tenant's hour-old sample is not a dead
-- reconciler. NULL on the rows written before this column: read as the hourly cadence.
ALTER TABLE status_samples ADD COLUMN next_within_secs BIGINT;
