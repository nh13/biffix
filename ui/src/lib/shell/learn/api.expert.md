# About the API

The HTTP API the UI and the `biffix` CLI use, as an OpenAPI document you can read here.

## How do I authenticate?

With a bearer token from your identity provider. `biffix login` gets one for the CLI.

## Is it stable?

Responses only gain fields; a client should ignore fields it doesn't know.

## Can I generate a client?

Yes, from the OpenAPI document this page renders.
