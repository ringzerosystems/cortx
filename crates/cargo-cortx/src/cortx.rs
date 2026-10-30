//! Standalone `cortx` CLI entrypoint.

#![forbid(unsafe_code)]

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    cargo_cortx::run().await
}
