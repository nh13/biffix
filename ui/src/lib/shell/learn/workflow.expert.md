# About this workflow

One registered version: its Run form, its About page, its parameters, every version of it, and its provenance (source commit, engine and pinned container images).

## How do I launch with different settings?

The **Run** tab is the full form. **Re-run** on a past run opens it with that run's parameters.

## What exactly is pinned?

The **Provenance** tab: the source repository and commit, the engine, and every container image by digest.

## How do I publish a new version?

Register it again from the new commit, then **Release this version** on its page (Developer or Admin; `biffix workflows release <id>` does the same) so the catalog offers it. Older versions stay registered, so their runs remain reproducible.
