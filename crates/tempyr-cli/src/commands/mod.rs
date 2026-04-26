use crate::config::ProjectContext;

pub mod add;
pub mod ask;
pub mod context;
pub mod dedupe;
pub mod dispatch;
pub mod doctor;
pub mod edge;
pub mod git_hooks;
pub mod import;
pub mod index_cmd;
pub mod init;
pub mod interview_cmd;
pub mod linear_cmd;
pub mod list;
pub mod managed;
pub mod migrate;
pub mod onboarding;
pub(crate) mod process_utils;
pub mod rename;
pub mod render_cmd;
pub mod search;
pub mod status_cmd;
pub mod traverse;
pub mod update;
pub mod validate;
pub mod vsearch;

pub(crate) fn warn_if_index_refresh_fails(ctx: &ProjectContext) {
    if let Err(err) = ctx.refresh_index_for_current_snapshot() {
        eprintln!("Warning: index refresh failed (run `tempyr index rebuild`): {err}");
    }
}
