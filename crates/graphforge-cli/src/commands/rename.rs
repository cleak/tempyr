use crate::config::ProjectContext;
use graphforge_core::ops;

pub fn run(ctx: &ProjectContext, old_id: &str, new_id: &str) -> anyhow::Result<()> {
    let modified = ops::rename_node(&ctx.graph_dir, old_id, new_id)?;
    println!("Renamed {old_id} -> {new_id}");
    println!("Modified {} file(s):", modified.len());
    for path in &modified {
        println!("  {}", path.display());
    }
    Ok(())
}
