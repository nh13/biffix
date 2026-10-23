//! Export the code-first OpenAPI document consumed by the web client build.

use std::io::{self, Write};

use biffix_backend::api::ApiDoc;
use utoipa::OpenApi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = ApiDoc::openapi().to_pretty_json()?;
    io::stdout().write_all(document.as_bytes())?;
    io::stdout().write_all(b"\n")?;
    Ok(())
}
