//! Prints the OpenAPI document to stdout.
//!
//! The server serves the same document at `/openapi.json`, but generating it
//! without booting anything — no database, no port — is what lets a client-SDK
//! codegen step run in CI:
//!
//! ```console
//! $ cargo run --example dump_openapi > openapi.json
//! ```

use utoipa::OpenApi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = <flagforge_api::openapi::ApiDoc as OpenApi>::openapi();
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}
