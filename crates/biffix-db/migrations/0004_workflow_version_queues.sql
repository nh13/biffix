-- Which worker queue-class each of a version's tasks runs on: the `queues:` block of its
-- biffix.yaml (`biffix_workflow::queues::QueueAssignment`), as validated at registration. `{}` for
-- a version that assigns none, every version registered before this column included.
ALTER TABLE workflow_versions ADD COLUMN queues JSONB NOT NULL DEFAULT '{}';
