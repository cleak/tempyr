pub mod handler;
mod shutdown;

use std::path::PathBuf;

use anyhow::{Result, bail};
use rmcp::service::{QuitReason, ServerInitializeError};
use rmcp::{ServiceExt, transport::stdio};

use shutdown::{ShutdownCoordinator, ShutdownReason};

pub use handler::TempyrServer;

pub async fn serve_stdio() -> Result<()> {
    serve_stdio_with_project_root_fallback(None).await
}

pub async fn serve_stdio_with_project_root_fallback(
    relative_project_root_fallback: Option<PathBuf>,
) -> Result<()> {
    tempyr_core::project::load_project_env()?;
    let shutdown = ShutdownCoordinator::new();
    shutdown.spawn_parent_watcher();

    let service = match TempyrServer::new()
        .with_relative_project_root_fallback(relative_project_root_fallback)
        .with_deferred_project_anchor()
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

    service
        .service()
        .try_anchor_from_client_roots(service.peer().clone())
        .await;
    service.service().mark_project_anchor_ready();

    match service.waiting().await? {
        QuitReason::Closed => shutdown.graceful_exit(ShutdownReason::StdinEof)?,
        QuitReason::Cancelled => shutdown.graceful_cancelled()?,
        QuitReason::JoinError(err) => bail!("MCP service task failed: {err}"),
        other => bail!("MCP service ended unexpectedly: {other:?}"),
    }

    Ok(())
}
