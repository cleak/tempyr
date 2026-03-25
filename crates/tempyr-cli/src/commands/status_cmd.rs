use crate::config::ProjectContext;
use tempyr_core::ops;

pub fn run(ctx: &ProjectContext, id: &str, new_status: &str) -> anyhow::Result<()> {
    ops::update_status(&ctx.graph_dir, id, new_status, &ctx.schema)?;
    println!("Updated {id} status to '{new_status}'");
    Ok(())
}
