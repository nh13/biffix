# About Workflows

Every registered workflow, one row each (its newest released version, the older ones folded under it), with its project, engine, queues and runs. A version is pinned to its source commit and container images, so a rerun is the same analysis.

## How do I add a workflow?

Register it with `biffix workflows register`. biffix reads its parameter schema and `biffix.yaml`, then resolves and pins its images.

## Why can't a version run yet?

Its images are still being prepared, or one failed to build. The version's **Provenance** tab shows each image and why.

## Which workflows are listed?

Those with a _Released_ version, each row the newest released one; turn on **Developer** (remembered in this browser) to list every workflow and its newest version. A Developer or Admin releases a version from its page, or with `biffix workflows release <id>`. The Lab view shows the same as cards, with the description, tags and _Verified_ badge (the author's own claim, unchecked) from its `biffix.yaml` `catalog:` section.
