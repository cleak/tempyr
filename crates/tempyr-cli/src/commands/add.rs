use crate::config::ProjectContext;
use tempyr_core::ops;

pub fn run(
    ctx: &ProjectContext,
    node_type: &str,
    slug: Option<&str>,
    id: Option<&str>,
    status: Option<&str>,
    owner: Option<&str>,
    body: Option<&str>,
) -> anyhow::Result<()> {
    match (slug, id) {
        (Some(slug), None) => {
            let default_body = format!("# {slug}\n");
            let body = body.unwrap_or(&default_body);
            let (generated_id, path) = ops::create_node_file_auto_id(
                &ctx.graph_dir, slug, node_type, status, owner, None, body,
            )?;
            println!("Created {generated_id} at {}", path.display());
        }
        (None, Some(id)) => {
            let default_body = format!("# {id}\n");
            let body = body.unwrap_or(&default_body);
            let path = ops::create_node_file(
                &ctx.graph_dir, id, node_type, status, owner, None, body,
            )?;
            println!("Created {id} at {}", path.display());
        }
        _ => anyhow::bail!("Provide either --slug (recommended) or --id"),
    }
    super::warn_if_index_refresh_fails(ctx);
    Ok(())
}
