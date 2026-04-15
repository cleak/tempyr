pub mod handler;
mod shutdown;

use anyhow::{Result, bail};
use rmcp::service::{QuitReason, ServerInitializeError};
use rmcp::{ServiceExt, transport::stdio};

use shutdown::{ShutdownCoordinator, ShutdownReason};

pub use handler::TempyrServer;

pub async fn serve_stdio() -> Result<()> {
    tempyr_core::project::load_project_env()?;
    let shutdown = ShutdownCoordinator::new();
    shutdown.spawn_parent_watcher();

    let service = match TempyrServer::default()
        .serve_with_ct(stdio(), shutdown.cancellation_token())
        .await
    {
        Ok(service) => service,
        Err(ServerInitializeError::ConnectionClosed(_)) => {
            return shutdown.graceful_exit(ShutdownReason::StdinEof);
        }
        Err(ServerInitializeError::Cancelled) => {
            return shutdown.graceful_cancelled();
        }
        Err(err) => return Err(err.into()),
    };

    match service.waiting().await? {
        QuitReason::Closed => shutdown.graceful_exit(ShutdownReason::StdinEof)?,
        QuitReason::Cancelled => shutdown.graceful_cancelled()?,
        QuitReason::JoinError(err) => bail!("MCP service task failed: {err}"),
        other => bail!("MCP service ended unexpectedly: {other:?}"),
    }

    Ok(())
}
