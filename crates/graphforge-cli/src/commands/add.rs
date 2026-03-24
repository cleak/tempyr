use crate::config::ProjectContext;
use graphforge_core::ops;

pub fn run(
    ctx: &ProjectContext,
    node_type: &str,
    id: &str,
    status: Option<&str>,
    owner: Option<&str>,
    body: Option<&str>,
) -> anyhow::Result<()> {
    let default_body = format!("# {id}\n");
    let body = body.unwrap_or(&default_body);

    let path = ops::create_node_file(
        &ctx.graph_dir,
        id,
        node_type,
        status,
        owner,
        None,
        body,
    )?;

    println!("Created {} at {}", id, path.display());
    Ok(())
}
