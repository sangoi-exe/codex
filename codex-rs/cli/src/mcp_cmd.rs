use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use clap::Args;
use codex_common::CliConfigOverrides;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core::config::find_codex_home;
use codex_core::config::load_global_mcp_servers;
use codex_core::config::write_global_mcp_servers;
use codex_core::config_types::McpServerConfig;
use toml::Value;

/// [experimental] Launch Codex as an MCP server or manage configured MCP servers.
///
/// Subcommands:
/// - `serve`  — run the MCP server on stdio
/// - `list`   — list configured servers (with `--json`)
/// - `get`    — show a single server (with `--json`)
/// - `add`    — add a server launcher entry to `~/.codex/config.toml`
/// - `remove` — delete a server entry
#[derive(Debug, clap::Parser)]
#[command(
    after_help = "When no subcommand is provided, `codex mcp` runs `serve` by default.",
    args_conflicts_with_subcommands = true,
    subcommand_precedence_over_arg = true
)]
pub struct McpCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub serve_args: ServeArgs,

    #[command(subcommand)]
    pub cmd: Option<McpSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
pub enum McpSubcommand {
    /// [experimental] Run the Codex MCP server (stdio transport).
    Serve(ServeCommandArgs),

    /// [experimental] List configured MCP servers.
    List(ListArgs),

    /// [experimental] Show details for a configured MCP server.
    Get(GetArgs),

    /// [experimental] Add a global MCP server entry.
    Add(AddArgs),

    /// [experimental] Remove a global MCP server entry.
    Remove(RemoveArgs),
}

#[derive(Debug, Args, Default, Clone)]
pub struct ServeArgs {
    /// Expose the complete Codex action surface as individually addressable MCP tools.
    #[arg(long, global = true)]
    pub expose_all_tools: bool,

    /// Enable auxiliary Codex agents (defaults to 2 concurrent agents unless overridden).
    #[arg(long, global = true)]
    pub enable_multiagent: bool,

    /// Maximum number of auxiliary Codex agents the server may orchestrate concurrently.
    #[arg(long, value_name = "N", global = true)]
    pub max_aux_agents: Option<usize>,
}

#[derive(Debug, clap::Parser, Default, Clone)]
#[command(
    after_help = "Runtime parameters belong to tool inputs via MCP; do not pass them as CLI flags."
)]
pub struct ServeCommandArgs {
    #[command(flatten)]
    pub flags: ServeArgs,

    /// Arguments after `--` are accepted for compatibility with MCP Inspector but ignored.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        num_args = 0..,
        value_name = "INSPECTOR_ARGS"
    )]
    pub passthrough: Vec<String>,
}

#[derive(Debug, clap::Parser)]
pub struct ListArgs {
    /// Output the configured servers as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Parser)]
pub struct GetArgs {
    /// Name of the MCP server to display.
    pub name: String,

    /// Output the server configuration as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Parser)]
pub struct AddArgs {
    /// Name for the MCP server configuration.
    pub name: String,

    /// Environment variables to set when launching the server.
    #[arg(long, value_parser = parse_env_pair, value_name = "KEY=VALUE")]
    pub env: Vec<(String, String)>,

    /// Command to launch the MCP server.
    #[arg(trailing_var_arg = true, num_args = 1..)]
    pub command: Vec<String>,
}

#[derive(Debug, clap::Parser)]
pub struct RemoveArgs {
    /// Name of the MCP server configuration to remove.
    pub name: String,
}

impl McpCli {
    pub async fn run(self, codex_linux_sandbox_exe: Option<PathBuf>) -> Result<()> {
        let McpCli {
            config_overrides,
            serve_args,
            cmd,
        } = self;

        match cmd {
            None => {
                let (effective_flags, passthrough) = finalize_serve_args(serve_args.clone(), None);
                run_serve(
                    &config_overrides,
                    effective_flags,
                    passthrough,
                    codex_linux_sandbox_exe.clone(),
                )
                .await?;
            }
            Some(McpSubcommand::Serve(sub_args)) => {
                let (effective_flags, passthrough) =
                    finalize_serve_args(serve_args.clone(), Some(sub_args));
                run_serve(
                    &config_overrides,
                    effective_flags,
                    passthrough,
                    codex_linux_sandbox_exe.clone(),
                )
                .await?;
            }
            Some(McpSubcommand::List(args)) => {
                warn_ignored_serve_flags(&serve_args, &[], "list");
                run_list(&config_overrides, args)?;
            }
            Some(McpSubcommand::Get(args)) => {
                warn_ignored_serve_flags(&serve_args, &[], "get");
                run_get(&config_overrides, args)?;
            }
            Some(McpSubcommand::Add(args)) => {
                warn_ignored_serve_flags(&serve_args, &[], "add");
                run_add(&config_overrides, args)?;
            }
            Some(McpSubcommand::Remove(args)) => {
                warn_ignored_serve_flags(&serve_args, &[], "remove");
                run_remove(&config_overrides, args)?;
            }
        }

        Ok(())
    }
}

fn run_add(config_overrides: &CliConfigOverrides, add_args: AddArgs) -> Result<()> {
    ensure_plain_overrides(config_overrides)?;

    let AddArgs { name, env, command } = add_args;

    validate_server_name(&name)?;

    let mut command_parts = command.into_iter();
    let command_bin = command_parts
        .next()
        .ok_or_else(|| anyhow!("command is required"))?;
    let command_args: Vec<String> = command_parts.collect();

    let env_map = if env.is_empty() {
        None
    } else {
        let mut map = HashMap::new();
        for (key, value) in env {
            map.insert(key, value);
        }
        Some(map)
    };

    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    let mut servers = load_global_mcp_servers(&codex_home)
        .with_context(|| format!("failed to load MCP servers from {}", codex_home.display()))?;

    let new_entry = McpServerConfig {
        command: command_bin,
        args: command_args,
        env: env_map,
        startup_timeout_ms: None,
    };

    servers.insert(name.clone(), new_entry);

    write_global_mcp_servers(&codex_home, &servers)
        .with_context(|| format!("failed to write MCP servers to {}", codex_home.display()))?;

    println!("Added global MCP server '{name}'.");

    Ok(())
}

fn run_remove(config_overrides: &CliConfigOverrides, remove_args: RemoveArgs) -> Result<()> {
    ensure_plain_overrides(config_overrides)?;

    let RemoveArgs { name } = remove_args;

    validate_server_name(&name)?;

    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    let mut servers = load_global_mcp_servers(&codex_home)
        .with_context(|| format!("failed to load MCP servers from {}", codex_home.display()))?;

    let removed = servers.remove(&name).is_some();

    if removed {
        write_global_mcp_servers(&codex_home, &servers)
            .with_context(|| format!("failed to write MCP servers to {}", codex_home.display()))?;
    }

    if removed {
        println!("Removed global MCP server '{name}'.");
    } else {
        println!("No MCP server named '{name}' found.");
    }

    Ok(())
}

fn run_list(config_overrides: &CliConfigOverrides, list_args: ListArgs) -> Result<()> {
    let overrides = plain_overrides_as_toml(config_overrides)?;
    let config = Config::load_with_cli_overrides(overrides, ConfigOverrides::default())
        .context("failed to load configuration")?;

    let mut entries: Vec<_> = config.mcp_servers.iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    if list_args.json {
        let json_entries: Vec<_> = entries
            .into_iter()
            .map(|(name, cfg)| {
                let env = cfg.env.as_ref().map(|env| {
                    env.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                });
                serde_json::json!({
                    "name": name,
                    "command": cfg.command,
                    "args": cfg.args,
                    "env": env,
                    "startup_timeout_ms": cfg.startup_timeout_ms,
                })
            })
            .collect();
        let output = serde_json::to_string_pretty(&json_entries)?;
        println!("{output}");
        return Ok(());
    }

    if entries.is_empty() {
        println!("No MCP servers configured yet. Try `codex mcp add my-tool -- my-command`.");
        return Ok(());
    }

    let mut rows: Vec<[String; 4]> = Vec::new();
    for (name, cfg) in entries {
        let args = if cfg.args.is_empty() {
            "-".to_string()
        } else {
            cfg.args.join(" ")
        };

        let env = match cfg.env.as_ref() {
            None => "-".to_string(),
            Some(map) if map.is_empty() => "-".to_string(),
            Some(map) => {
                let mut pairs: Vec<_> = map.iter().collect();
                pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
                pairs
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };

        rows.push([name.clone(), cfg.command.clone(), args, env]);
    }

    let mut widths = ["Name".len(), "Command".len(), "Args".len(), "Env".len()];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    println!(
        "{:<name_w$}  {:<cmd_w$}  {:<args_w$}  {:<env_w$}",
        "Name",
        "Command",
        "Args",
        "Env",
        name_w = widths[0],
        cmd_w = widths[1],
        args_w = widths[2],
        env_w = widths[3],
    );

    for row in rows {
        println!(
            "{:<name_w$}  {:<cmd_w$}  {:<args_w$}  {:<env_w$}",
            row[0],
            row[1],
            row[2],
            row[3],
            name_w = widths[0],
            cmd_w = widths[1],
            args_w = widths[2],
            env_w = widths[3],
        );
    }

    Ok(())
}

fn run_get(config_overrides: &CliConfigOverrides, get_args: GetArgs) -> Result<()> {
    let overrides = plain_overrides_as_toml(config_overrides)?;
    let config = Config::load_with_cli_overrides(overrides, ConfigOverrides::default())
        .context("failed to load configuration")?;

    let Some(server) = config.mcp_servers.get(&get_args.name) else {
        bail!("No MCP server named '{name}' found.", name = get_args.name);
    };

    if get_args.json {
        let env = server.env.as_ref().map(|env| {
            env.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        });
        let output = serde_json::to_string_pretty(&serde_json::json!({
            "name": get_args.name,
            "command": server.command,
            "args": server.args,
            "env": env,
            "startup_timeout_ms": server.startup_timeout_ms,
        }))?;
        println!("{output}");
        return Ok(());
    }

    println!("{}", get_args.name);
    println!("  command: {}", server.command);
    let args = if server.args.is_empty() {
        "-".to_string()
    } else {
        server.args.join(" ")
    };
    println!("  args: {args}");
    let env_display = match server.env.as_ref() {
        None => "-".to_string(),
        Some(map) if map.is_empty() => "-".to_string(),
        Some(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
            pairs
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    println!("  env: {env_display}");
    if let Some(timeout) = server.startup_timeout_ms {
        println!("  startup_timeout_ms: {timeout}");
    }
    println!("  remove: codex mcp remove {}", get_args.name);

    Ok(())
}

async fn run_serve(
    config_overrides: &CliConfigOverrides,
    serve_flags: ServeArgs,
    passthrough: Vec<String>,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> Result<()> {
    let overrides = parse_plain_overrides(config_overrides)?;

    let ServeArgs {
        expose_all_tools,
        enable_multiagent,
        max_aux_agents,
    } = serve_flags;

    let effective_max_aux = match (enable_multiagent, max_aux_agents) {
        (false, _) => Some(0),
        (true, Some(limit)) => Some(limit),
        (true, None) => Some(2),
    };

    eprintln!(
        "[mcp] expose_all_tools={expose_all_tools} enable_multiagent={enable_multiagent} max_aux_agents={:?}",
        effective_max_aux
    );
    if !passthrough.is_empty() {
        eprintln!("[mcp] passthrough args ignored: {passthrough:?}");
    }

    let run_options = codex_mcp_server::McpServerRunOptions {
        opts: codex_mcp_server::McpServerOpts {
            expose_all_tools,
            overrides,
        },
        max_aux_agents: effective_max_aux,
    };

    codex_mcp_server::run_main(codex_linux_sandbox_exe, run_options)
        .await
        .map_err(|e| anyhow!(e))
}

fn finalize_serve_args(
    global: ServeArgs,
    subcommand: Option<ServeCommandArgs>,
) -> (ServeArgs, Vec<String>) {
    match subcommand {
        Some(sub_args) => (
            ServeArgs {
                expose_all_tools: global.expose_all_tools || sub_args.flags.expose_all_tools,
                enable_multiagent: global.enable_multiagent || sub_args.flags.enable_multiagent,
                max_aux_agents: sub_args.flags.max_aux_agents.or(global.max_aux_agents),
            },
            sub_args.passthrough,
        ),
        None => (global, Vec::new()),
    }
}

fn warn_ignored_serve_flags(args: &ServeArgs, passthrough: &[String], subcommand: &str) {
    let mut ignored_flags = Vec::new();
    if args.expose_all_tools {
        ignored_flags.push("--expose-all-tools");
    }
    if args.enable_multiagent {
        ignored_flags.push("--enable-multiagent");
    }
    if args.max_aux_agents.is_some() {
        ignored_flags.push("--max-aux-agents");
    }

    if !ignored_flags.is_empty() {
        eprintln!("[mcp] warning: {ignored_flags:?} ignored for `codex mcp {subcommand}`");
    }

    if !passthrough.is_empty() {
        eprintln!(
            "[mcp] warning: passthrough arguments ignored for `codex mcp {subcommand}`: {:?}",
            passthrough
        );
    }
}

fn parse_plain_overrides(overrides: &CliConfigOverrides) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for raw in &overrides.raw_overrides {
        let mut parts = raw.splitn(2, '=');
        let key = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(format!(
                    "invalid config override '{raw}': expected key=value"
                ))
            })?;
        let value = parts
            .next()
            .ok_or_else(|| {
                anyhow!(format!(
                    "invalid config override '{raw}': expected key=value"
                ))
            })?
            .to_string();

        map.insert(key.to_string(), value);
    }

    Ok(map)
}

fn ensure_plain_overrides(overrides: &CliConfigOverrides) -> Result<()> {
    parse_plain_overrides(overrides).map(|_| ())
}

fn plain_overrides_as_toml(overrides: &CliConfigOverrides) -> Result<Vec<(String, Value)>> {
    let mut entries: Vec<(String, Value)> = parse_plain_overrides(overrides)?
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

fn parse_env_pair(raw: &str) -> Result<(String, String), String> {
    let mut parts = raw.splitn(2, '=');
    let key = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "environment entries must be in KEY=VALUE form".to_string())?;
    let value = parts
        .next()
        .map(str::to_string)
        .ok_or_else(|| "environment entries must be in KEY=VALUE form".to_string())?;

    Ok((key.to_string(), value))
}

fn validate_server_name(name: &str) -> Result<()> {
    let is_valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');

    if is_valid {
        Ok(())
    } else {
        bail!("invalid server name '{name}' (use letters, numbers, '-', '_')");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    fn overrides_from(raw: &[&str]) -> CliConfigOverrides {
        CliConfigOverrides {
            raw_overrides: raw.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn top_level_flags_default_to_serve() {
        let cli =
            McpCli::try_parse_from(["mcp", "--expose-all-tools", "--enable-multiagent"]).expect("parse");
        assert!(cli.cmd.is_none());
        assert!(cli.serve_args.expose_all_tools);
        assert!(cli.serve_args.enable_multiagent);
    }

    #[test]
    fn multiagent_disabled_by_default() {
        let cli = McpCli::try_parse_from(["mcp"]).expect("parse");
        assert!(!cli.serve_args.enable_multiagent);
        assert_eq!(cli.serve_args.max_aux_agents, None);
    }

    #[test]
    fn max_aux_agents_flag_parses() {
        let cli = McpCli::try_parse_from(["mcp", "--enable-multiagent", "--max-aux-agents", "5"]).expect("parse");
        assert!(cli.serve_args.enable_multiagent);
        assert_eq!(cli.serve_args.max_aux_agents, Some(5));
    }

    #[test]
    fn serve_subcommand_preserves_flags() {
        let cli = McpCli::try_parse_from([
            "mcp",
            "serve",
            "--expose-all-tools",
            "--enable-multiagent",
        ])
        .expect("parse");
        match cli.cmd {
            Some(McpSubcommand::Serve(args)) => {
                assert!(args.flags.expose_all_tools);
                assert!(args.flags.enable_multiagent);
            }
            other => panic!("expected serve subcommand, got {other:?}"),
        }
    }

    #[test]
    fn passthrough_captured_for_serve() {
        let cli = McpCli::try_parse_from(["mcp", "serve", "--", "--cli", "--method", "tools/list"])
            .expect("parse");
        match cli.cmd {
            Some(McpSubcommand::Serve(args)) => {
                assert_eq!(
                    args.passthrough,
                    vec![
                        "--cli".to_string(),
                        "--method".to_string(),
                        "tools/list".to_string(),
                    ]
                );
            }
            other => panic!("expected serve subcommand, got {other:?}"),
        }
    }

    #[test]
    fn list_subcommand_tolerates_serve_flags() {
        let cli = McpCli::try_parse_from(["mcp", "list", "--expose-all-tools"]).expect("parse");
        assert!(matches!(cli.cmd, Some(McpSubcommand::List(_))));
        assert!(cli.serve_args.expose_all_tools);
    }

    #[test]
    fn finalize_serve_args_combines_global_and_sub_flags() {
        let mut global = ServeArgs::default();
        global.expose_all_tools = true;

        let mut sub = ServeCommandArgs::default();
        sub.flags.expose_all_tools = true;
        sub.flags.enable_multiagent = true;
        sub.flags.max_aux_agents = Some(3);

        let (combined_flags, passthrough) =
            finalize_serve_args(global.clone(), Some(sub.clone()));
        assert!(combined_flags.expose_all_tools);
        assert!(combined_flags.enable_multiagent);
        assert_eq!(combined_flags.max_aux_agents, Some(3));
        assert!(passthrough.is_empty());

        let mut sub_passthrough = ServeCommandArgs::default();
        sub_passthrough.flags.enable_multiagent = true;
        sub_passthrough.passthrough = vec!["--method".to_string()];
        let (_combined_flags, combined_passthrough) =
            finalize_serve_args(global.clone(), Some(sub_passthrough.clone()));
        assert_eq!(combined_passthrough, sub_passthrough.passthrough);
    }

    #[test]
    fn parse_plain_overrides_handles_basic_pairs() {
        let overrides = overrides_from(&["model=o3", "foo.bar=baz"]);
        let map = parse_plain_overrides(&overrides).expect("parse overrides");

        assert_eq!(map.get("model"), Some(&"o3".to_string()));
        assert_eq!(map.get("foo.bar"), Some(&"baz".to_string()));
    }

    #[test]
    fn parse_plain_overrides_rejects_missing_equals() {
        let overrides = overrides_from(&["missing"]);
        let err = parse_plain_overrides(&overrides).expect_err("should fail");
        assert!(err.to_string().contains("expected key=value"));
    }

    #[test]
    fn plain_overrides_are_sorted_and_stringified() {
        let overrides = overrides_from(&["beta=1", "alpha=two"]);
        let entries = plain_overrides_as_toml(&overrides).expect("convert overrides");
        assert_eq!(
            entries,
            vec![
                ("alpha".to_string(), Value::String("two".to_string())),
                ("beta".to_string(), Value::String("1".to_string())),
            ]
        );
    }
}
