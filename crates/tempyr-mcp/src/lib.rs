pub mod handler;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};

pub use handler::TempyrServer;

pub async fn serve_stdio() -> Result<()> {
    tempyr_core::project::load_project_env()?;
    let service = TempyrServer::default().serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
