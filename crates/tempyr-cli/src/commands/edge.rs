use crate::config::ProjectContext;
use tempyr_core::ops;

pub fn run_add(
    ctx: &ProjectContext,
    source: &str,
    target: &str,
    edge_type: &str,
) -> anyhow::Result<()> {
    ops::add_edge(&ctx.graph_dir, source, target, edge_type, &ctx.schema)?;
    let reverse = ctx.schema.reverse_edge_type(edge_type).unwrap_or("?");
    println!("Added edge: {source} --{edge_type}--> {target}");
    println!("Added reverse: {target} --{reverse}--> {source}");
    super::warn_if_index_refresh_fails(ctx);
    Ok(())
}

pub fn run_remove(
    ctx: &ProjectContext,
    source: &str,
    target: &str,
    edge_type: &str,
) -> anyhow::Result<()> {
    ops::remove_edge(&ctx.graph_dir, source, target, edge_type, &ctx.schema)?;
    println!("Removed edge: {source} --{edge_type}--> {target} (and reverse)");
    super::warn_if_index_refresh_fails(ctx);
    Ok(())
}
