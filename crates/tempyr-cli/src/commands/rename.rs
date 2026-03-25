use crate::config::ProjectContext;
use tempyr_core::ops;

pub fn run(
    ctx: &ProjectContext,
    old_id: &str,
    new_id: Option<&str>,
    new_slug: Option<&str>,
) -> anyhow::Result<()> {
    let modified = match (new_id, new_slug) {
        (Some(new_id), None) => {
            let files = ops::rename_node(&ctx.graph_dir, old_id, new_id)?;
            println!("Renamed {old_id} -> {new_id}");
            files
        }
        (None, Some(slug)) => {
            let files = ops::rename_node_slug(&ctx.graph_dir, old_id, slug)?;
            let parsed = tempyr_core::id::parse_node_id(old_id)
                .expect("slug rename requires hybrid ID");
            println!("Renamed {old_id} -> {slug}-{}", parsed.suffix);
            files
        }
        _ => anyhow::bail!("Provide either a new full ID or --slug <new-slug>"),
    };

    println!("Modified {} file(s):", modified.len());
    for path in &modified {
        println!("  {}", path.display());
    }
    Ok(())
}
