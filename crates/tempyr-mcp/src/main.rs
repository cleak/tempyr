mod handler;

use rmcp::{ServiceExt, transport::stdio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("tempyr-mcp: MCP server starting on stdio");

    let service = handler::TempyrServer::new().serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
