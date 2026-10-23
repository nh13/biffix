# About References

Reference bundles: a named set of files, such as a genome FASTA, its index and its annotation, that fills a workflow's reference parameters when chosen in the launch form.

## How do I add one?

**Add a reference**, then give it a name, a kind and the `s3://` path of each file by key.

## How does a workflow use it?

A workflow's `biffix.yaml` maps bundle keys to its own parameters. The launch form offers only bundles with every key it requires.

## Can I change one that is in use?

Yes. Runs already launched keep the paths they started with.
