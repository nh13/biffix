# About the Run form

Every parameter of this workflow, from its schema, in sections. Required fields are marked; problems are listed beside **Launch run** and block it until fixed.

## Where do inputs come from?

**Browse…** opens the file picker over the configured data sources and your project bucket, where you can also upload. Or paste an `s3://` path.

## How are references filled?

Choosing a registered reference bundle fills the parameters it covers. When the workflow allows it, **Use my own files…** shows those parameters instead.

## Can I submit from the command line?

Yes: `biffix runs submit` takes the same parameters.
