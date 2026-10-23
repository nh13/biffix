# About Status

The health of each part of biffix: the API, the database, event ingest, AWS Batch, the background sync and the alarms, sampled on a schedule.

## What does degraded mean?

A part is slow or partly failing. Runs may still launch, but their progress can show late.

## Who is told?

The alert recipients under Settings get an email when an alarm fires.

## What should I check first?

The failing part's message, then event ingest: if it is behind, runs' progress is waiting to be shown.
