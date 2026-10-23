# About this run

The run's overview, with tabs for the task graph, every AWS Batch job and its logs, the coordinator's own log, cost, the parameters it ran with, its reports and its outputs.

## Why did a job fail?

The **Jobs** tab lists every attempt with its exit code and status reason; open one for its log. A spot reclaim is retried and shows as another attempt.

## How do I rerun from where it stopped?

**Resume**, when offered, starts a new run from this one's cache: finished tasks are reused and only the rest run again.

## Where does the cost come from?

Each job's instance time at its instance type's on-demand or spot price, plus its EBS volume; the coordinator job counts too. The **Cost** tab breaks it down.
