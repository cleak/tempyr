use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_index::indexer::Index;
use tempyr_linear::client::LinearClient;
use tempyr_linear::config::LinearConfig;
use tempyr_linear::mapping::StatusMapper;
use tempyr_linear::queries::*;
use tempyr_linear::state::{SyncEntry, SyncState};
use tempyr_linear::sync;

use chrono::Utc;
use serde_json::json;

fn rt() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().map_err(|e| anyhow::anyhow!("Failed to create runtime: {e}"))
}

fn finalize_linear_graph_update(
    ctx: &ProjectContext,
    state: &SyncState,
    graph_changed: bool,
) -> anyhow::Result<()> {
    if graph_changed {
        super::warn_if_index_refresh_fails(ctx);
    }
    state.save(&ctx.tempyr_dir)?;
    Ok(())
}

fn load_optional_queryable_index(ctx: &ProjectContext) -> Option<Index> {
    let path = match ctx.queryable_index_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("Warning: failed to prepare index-backed Linear context: {err}");
            return None;
        }
    };

    match Index::open(&path) {
        Ok(index) => Some(index),
        Err(err) => {
            eprintln!(
                "Warning: failed to open index-backed Linear context at {}: {err}",
                path.display()
            );
            None
        }
    }
}

pub fn run_setup(ctx: &ProjectContext, json_output: bool) -> anyhow::Result<()> {
    let client = LinearClient::from_env()?;
    let runtime = rt()?;

    runtime.block_on(async {
        // Verify API key
        let viewer: ViewerData = client
            .execute(VIEWER_QUERY, serde_json::Value::Null)
            .await?;
        if !json_output {
            println!(
                "Authenticated as: {} ({})",
                viewer.viewer.name, viewer.viewer.email
            );
        }

        // List teams
        let teams_data: TeamsData = client.execute(TEAMS_QUERY, serde_json::Value::Null).await?;

        let teams = &teams_data.teams.nodes;
        if teams.is_empty() {
            anyhow::bail!("No teams found in your Linear workspace");
        }

        // Auto-select if only one team
        let team = if teams.len() == 1 {
            &teams[0]
        } else {
            if !json_output {
                println!("\nAvailable teams:");
                for (i, t) in teams.iter().enumerate() {
                    println!("  {}. {} ({})", i + 1, t.name, t.key);
                }
                println!("\nUsing first team. To change, edit .tempyr/linear.json");
            }
            &teams[0]
        };

        if !json_output {
            println!("Selected team: {} ({})", team.name, team.key);
        }

        // Fetch workflow states for the team
        let states_data: WorkflowStatesData = client
            .execute(WORKFLOW_STATES_QUERY, json!({ "teamId": team.id }))
            .await?;

        let mut workflow_states = std::collections::HashMap::new();
        if !json_output {
            println!("\nWorkflow states:");
        }
        for state in &states_data.workflow_states.nodes {
            if !json_output {
                println!("  {} (type: {})", state.name, state.state_type);
            }
            workflow_states.insert(state.name.clone(), state.id.clone());
        }

        // Save config
        let config = LinearConfig {
            team_id: team.id.clone(),
            team_name: team.name.clone(),
            team_key: team.key.clone(),
            default_project_id: None,
            workflow_states,
            status_overrides: std::collections::HashMap::new(),
        };
        config.save(&ctx.tempyr_dir)?;

        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "success": true,
                    "user": viewer.viewer.name,
                    "team": team.name,
                    "team_key": team.key,
                    "states": states_data.workflow_states.nodes.len(),
                }))?
            );
        } else {
            println!("\nLinear integration configured. Config saved to .tempyr/linear.json");
            println!("Run `tempyr linear push` to sync nodes to Linear.");
        }

        Ok(())
    })
}

pub fn run_push(
    ctx: &ProjectContext,
    node_id: Option<&str>,
    dry_run: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    let client = LinearClient::from_env()?;
    let config = LinearConfig::load(&ctx.tempyr_dir)?;
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let mut state = SyncState::load(&ctx.tempyr_dir)?;
    let status_mapper = build_status_mapper(&client, &config)?;

    if dry_run {
        return run_push_dry_run(&graph, &state, node_id, json_output);
    }

    let index = load_optional_queryable_index(ctx);
    let runtime = rt()?;
    runtime.block_on(async {
        if let Some(id) = node_id {
            let node = graph
                .get_node(id)
                .ok_or_else(|| anyhow::anyhow!("Node not found: {id}"))?;
            let entry = tempyr_linear::push::push_node(
                &client,
                node,
                &graph,
                index.as_ref(),
                &ctx.schema,
                &config,
                &mut state,
                &status_mapper,
            )
            .await?;
            state.save(&ctx.tempyr_dir)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "pushed": [{
                            "node_id": entry.node_id,
                            "linear_id": entry.linear_id,
                            "linear_identifier": entry.linear_identifier,
                        }]
                    }))?
                );
            } else {
                let action = match entry.action {
                    tempyr_linear::push::PushAction::Created => "Created",
                    tempyr_linear::push::PushAction::Updated => "Updated",
                };
                let ident = entry
                    .linear_identifier
                    .as_deref()
                    .unwrap_or(&entry.linear_id);
                println!("{action} {ident} <- {}", entry.node_id);
            }
            Ok(())
        } else {
            let result = tempyr_linear::push::push_all(
                &client,
                &graph,
                index.as_ref(),
                &ctx.schema,
                &config,
                &mut state,
                &status_mapper,
            )
            .await?;
            state.save(&ctx.tempyr_dir)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "created": result.created.len(),
                        "updated": result.updated.len(),
                        "skipped": result.skipped.len(),
                        "errors": result.errors.len(),
                    }))?
                );
            } else {
                for e in &result.created {
                    let ident = e.linear_identifier.as_deref().unwrap_or(&e.linear_id);
                    println!("  + {ident} <- {}", e.node_id);
                }
                for e in &result.updated {
                    let ident = e.linear_identifier.as_deref().unwrap_or(&e.linear_id);
                    println!("  ~ {ident} <- {}", e.node_id);
                }
                for (id, err) in &result.errors {
                    eprintln!("  ! {id}: {err}");
                }
                println!(
                    "\nPushed: {} created, {} updated, {} skipped, {} errors",
                    result.created.len(),
                    result.updated.len(),
                    result.skipped.len(),
                    result.errors.len()
                );
            }
            Ok(())
        }
    })
}

pub fn run_pull(ctx: &ProjectContext, dry_run: bool, json_output: bool) -> anyhow::Result<()> {
    let client = LinearClient::from_env()?;
    let config = LinearConfig::load(&ctx.tempyr_dir)?;
    let mut state = SyncState::load(&ctx.tempyr_dir)?;
    let status_mapper = build_status_mapper(&client, &config)?;

    if dry_run {
        if !json_output {
            println!(
                "Dry run: would poll Linear for changes since {:?}",
                state.last_sync_at
            );
            println!("  {} tracked entries to check", state.entries.len());
        }
        return Ok(());
    }

    let runtime = rt()?;
    runtime.block_on(async {
        let result = tempyr_linear::pull::pull(
            &client,
            &ctx.graph_dir,
            &ctx.schema,
            &config,
            &mut state,
            &status_mapper,
        )
        .await?;
        finalize_linear_graph_update(ctx, &state, result.changed_graph())?;

        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "created": result.created,
                    "updated": result.updated,
                    "status_changed": result.status_changed.len(),
                    "conflicts": result.conflicts.len(),
                    "errors": result.errors.len(),
                }))?
            );
        } else {
            for id in &result.created {
                println!("  + Created node: {id}");
            }
            for sc in &result.status_changed {
                println!(
                    "  ~ {} status: {} -> {}",
                    sc.node_id, sc.old_status, sc.new_status
                );
            }
            for c in &result.conflicts {
                eprintln!("  ! Conflict: {} ({})", c.node_id, c.reason);
            }
            for w in &result.warnings {
                eprintln!("  ? {w}");
            }
            for (id, err) in &result.errors {
                eprintln!("  ! {id}: {err}");
            }
            println!(
                "\nPulled: {} created, {} updated, {} conflicts, {} errors",
                result.created.len(),
                result.updated.len(),
                result.conflicts.len(),
                result.errors.len()
            );
        }
        Ok(())
    })
}

pub fn run_sync(ctx: &ProjectContext, dry_run: bool, json_output: bool) -> anyhow::Result<()> {
    let client = LinearClient::from_env()?;
    let config = LinearConfig::load(&ctx.tempyr_dir)?;
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let mut state = SyncState::load(&ctx.tempyr_dir)?;
    let status_mapper = build_status_mapper(&client, &config)?;

    if dry_run {
        return run_sync_dry_run(&graph, &state, json_output);
    }

    let index = load_optional_queryable_index(ctx);
    let runtime = rt()?;
    runtime.block_on(async {
        let result = sync::sync(
            &client,
            &ctx.graph_dir,
            &graph,
            index.as_ref(),
            &ctx.schema,
            &config,
            &mut state,
            &status_mapper,
        )
        .await?;
        finalize_linear_graph_update(ctx, &state, result.changed_graph())?;

        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "push": {
                        "created": result.push.created.len(),
                        "updated": result.push.updated.len(),
                        "errors": result.push.errors.len(),
                    },
                    "pull": {
                        "created": result.pull.created.len(),
                        "updated": result.pull.updated.len(),
                        "conflicts": result.pull.conflicts.len(),
                        "errors": result.pull.errors.len(),
                    }
                }))?
            );
        } else {
            println!(
                "Push: {} created, {} updated, {} errors",
                result.push.created.len(),
                result.push.updated.len(),
                result.push.errors.len()
            );
            println!(
                "Pull: {} created, {} updated, {} conflicts, {} errors",
                result.pull.created.len(),
                result.pull.updated.len(),
                result.pull.conflicts.len(),
                result.pull.errors.len()
            );
        }
        Ok(())
    })
}

pub fn run_status(ctx: &ProjectContext, json_output: bool) -> anyhow::Result<()> {
    let config_result = LinearConfig::load(&ctx.tempyr_dir);
    if config_result.is_err() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "configured": false }))?
            );
        } else {
            println!("Linear integration not configured. Run `tempyr linear setup` first.");
        }
        return Ok(());
    }

    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let state = SyncState::load(&ctx.tempyr_dir)?;
    let report = sync::status_summary(&state, &graph);

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Linear sync status:");
        println!("  Linked:    {}", report.linked_count);
        println!(
            "  Unlinked:  {} (syncable nodes not yet pushed)",
            report.unlinked_syncable_count
        );
        println!(
            "  Stale:     {} (local changes pending push)",
            report.stale_count
        );
        println!(
            "  Orphaned:  {} (linked but node deleted)",
            report.orphaned_count
        );
        if let Some(last) = report.last_sync {
            println!("  Last sync: {}", last.format("%Y-%m-%d %H:%M:%S UTC"));
        } else {
            println!("  Last sync: never");
        }

        if !report.entries.is_empty() {
            println!("\nLinked nodes:");
            for e in &report.entries {
                let stale = if e.is_stale { " [stale]" } else { "" };
                let ident = e.linear_identifier.as_deref().unwrap_or(&e.linear_id);
                println!("  {} ({}) <-> {}{stale}", e.node_id, e.node_type, ident);
            }
        }
    }

    Ok(())
}

pub fn run_link(ctx: &ProjectContext, node_id: &str, linear_id: &str) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let node = graph
        .get_node(node_id)
        .ok_or_else(|| anyhow::anyhow!("Node not found: {node_id}"))?;

    let node_type = node.node_type().to_string();
    if !matches!(node_type.as_str(), "epic" | "feature" | "task") {
        anyhow::bail!("Only epic, feature, and task nodes can be linked to Linear");
    }

    let mut state = SyncState::load(&ctx.tempyr_dir)?;
    let now = Utc::now();

    state.upsert(SyncEntry {
        node_id: node_id.to_string(),
        linear_id: linear_id.to_string(),
        linear_identifier: None,
        node_type,
        content_hash_at_sync: node.content_hash.clone(),
        linear_updated_at: now,
        last_synced_at: now,
        attachment_ids: vec![],
    });
    state.save(&ctx.tempyr_dir)?;

    println!("Linked {node_id} <-> {linear_id}");
    Ok(())
}

pub fn run_unlink(ctx: &ProjectContext, node_id: &str) -> anyhow::Result<()> {
    let mut state = SyncState::load(&ctx.tempyr_dir)?;

    if state.remove_by_node_id(node_id).is_some() {
        state.save(&ctx.tempyr_dir)?;
        println!("Unlinked {node_id} from Linear");
    } else {
        println!("Node {node_id} was not linked to Linear");
    }

    Ok(())
}

// Helpers

fn build_status_mapper(
    client: &LinearClient,
    config: &LinearConfig,
) -> anyhow::Result<StatusMapper> {
    let states: Vec<WorkflowState> = config
        .workflow_states
        .iter()
        .map(|(name, id)| WorkflowState {
            id: id.clone(),
            name: name.clone(),
            state_type: String::new(),
        })
        .collect();

    if states.is_empty() {
        // Fetch from API if not cached in config
        let runtime = rt()?;
        let states = runtime.block_on(async {
            let data: WorkflowStatesData = client
                .execute(WORKFLOW_STATES_QUERY, json!({ "teamId": config.team_id }))
                .await?;
            Ok::<_, anyhow::Error>(data.workflow_states.nodes)
        })?;
        Ok(StatusMapper::new(states))
    } else {
        Ok(StatusMapper::new(states))
    }
}

fn run_push_dry_run(
    graph: &Graph,
    state: &SyncState,
    node_id: Option<&str>,
    json_output: bool,
) -> anyhow::Result<()> {
    let syncable_types = ["epic", "feature", "task"];

    if let Some(id) = node_id {
        let node = graph
            .get_node(id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {id}"))?;
        let linked = state.get_by_node_id(id).is_some();
        let stale = state
            .get_by_node_id(id)
            .is_some_and(|e| e.content_hash_at_sync != node.content_hash);

        if !json_output {
            if !linked {
                println!("  Would create: {id} ({})", node.node_type());
            } else if stale {
                println!("  Would update: {id} ({})", node.node_type());
            } else {
                println!("  No changes: {id}");
            }
        }
    } else {
        let mut would_create = 0;
        let mut would_update = 0;
        let mut would_skip = 0;

        for node in graph.nodes.values() {
            if !syncable_types.contains(&node.node_type()) {
                continue;
            }
            if let Some(entry) = state.get_by_node_id(node.id()) {
                if node.content_hash != entry.content_hash_at_sync {
                    if !json_output {
                        println!("  ~ Would update: {} ({})", node.id(), node.node_type());
                    }
                    would_update += 1;
                } else {
                    would_skip += 1;
                }
            } else {
                if !json_output {
                    println!("  + Would create: {} ({})", node.id(), node.node_type());
                }
                would_create += 1;
            }
        }

        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "dry_run": true,
                    "would_create": would_create,
                    "would_update": would_update,
                    "would_skip": would_skip,
                }))?
            );
        } else {
            println!("\nDry run: {would_create} create, {would_update} update, {would_skip} skip");
        }
    }

    Ok(())
}

fn run_sync_dry_run(graph: &Graph, state: &SyncState, json_output: bool) -> anyhow::Result<()> {
    let report = sync::status_summary(state, graph);

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": true,
                "would_push": report.stale_count + report.unlinked_syncable_count,
                "tracked": report.linked_count,
            }))?
        );
    } else {
        println!("Dry run sync:");
        println!(
            "  Would push: {} nodes ({} new, {} updated)",
            report.stale_count + report.unlinked_syncable_count,
            report.unlinked_syncable_count,
            report.stale_count
        );
        println!(
            "  Would poll Linear for {} tracked entries",
            report.linked_count
        );
    }

    Ok(())
}
