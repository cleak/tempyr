pub mod handler;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};

pub use handler::TempyrServer;

pub async fn serve_stdio() -> Result<()> {
    let service = TempyrServer::default().serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
