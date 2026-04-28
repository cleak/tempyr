pub mod handler;
mod journal_ticker;
mod shutdown;

use std::path::PathBuf;

use anyhow::{Result, bail};
use rmcp::service::{QuitReason, ServerInitializeError};
use rmcp::{ServiceExt, transport::stdio};

use shutdown::{ShutdownCoordinator, ShutdownReason};

pub use handler::TempyrServer;
pub use journal_ticker::SpawnOutcome as JournalTickerOutcome;

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

    // Spawn the in-process publisher ticker now that the project anchor
    // has settled. It runs every TEMPYR_JOURNAL_TICK_SECS (default 60s)
    // for the lifetime of the MCP service, plus one final flush when
    // the cancellation token fires. If the project root isn't a git
    // repo, the ticker silently no-ops — journals are git-only.
    if let Some(project_root) = tempyr_core::project::find_project_root() {
        match journal_ticker::spawn(&project_root, shutdown.cancellation_token()) {
            JournalTickerOutcome::Running { interval, .. } => {
                eprintln!("tempyr journal ticker: every {}s", interval.as_secs());
            }
            JournalTickerOutcome::Disabled => {
                eprintln!("tempyr journal ticker: disabled in config");
            }
            JournalTickerOutcome::NotAGitRepo => {
                eprintln!("tempyr journal ticker: not a git repo, skipping");
            }
            JournalTickerOutcome::Unavailable(msg) => {
                eprintln!("tempyr journal ticker: unavailable ({msg})");
            }
        }
    }

    match service.waiting().await? {
        QuitReason::Closed => shutdown.graceful_exit(ShutdownReason::StdinEof)?,
        QuitReason::Cancelled => shutdown.graceful_cancelled()?,
        QuitReason::JoinError(err) => bail!("MCP service task failed: {err}"),
        other => bail!("MCP service ended unexpectedly: {other:?}"),
    }

    Ok(())
}
