use std::path::PathBuf;
use std::time::Duration;

use codex_apply_patch::apply_patch as run_apply_patch;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use mcp_types::TextContent;
use mcp_types::Tool;
use mcp_types::ToolInputSchema;
use schemars::JsonSchema;
use schemars::r#gen::SchemaSettings;
use schemars::schema::RootSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;
use tokio::task;
use tracing::error;
// no chrono needed here anymore

// -------------------- helpers --------------------
fn ok(text: String) -> CallToolResult {
    CallToolResult {
        content: vec![ContentBlock::TextContent(TextContent {
            r#type: "text".into(),
            text,
            annotations: None,
        })],
        is_error: Some(false),
        structured_content: None,
    }
}
fn err(text: String) -> CallToolResult {
    CallToolResult {
        content: vec![ContentBlock::TextContent(TextContent {
            r#type: "text".into(),
            text,
            annotations: None,
        })],
        is_error: Some(true),
        structured_content: None,
    }
}
fn trunc_utf8(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    s.truncate(i);
    s.push_str("\n... [truncated]\n");
    s
}

fn to_tool_input_schema(tool_name: &str, schema: RootSchema) -> ToolInputSchema {
    match serde_json::to_value(&schema).and_then(serde_json::from_value::<ToolInputSchema>) {
        Ok(input_schema) => input_schema,
        Err(e) => {
            error!("failed to build input schema for {tool_name}: {e}");
            ToolInputSchema {
                r#type: "object".into(),
                properties: None,
                required: None,
            }
        }
    }
}

// -------------------- chatgpt.astGrep --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AstGrepParams {
    #[serde(default)]
    pub raw_args: Option<Vec<String>>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub json: Option<bool>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}
fn tool_astgrep_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .with(|s| {
            s.inline_subschemas = true;
            #[allow(deprecated)]
            {
                s.option_add_null_type = false;
                s.option_nullable = false;
            }
        })
        .into_generator()
        .into_root_schema_for::<AstGrepParams>();
    let input_schema = to_tool_input_schema("chatgpt.astGrep", schema);
    Tool {
        name: "chatgpt.astGrep".into(),
        title: Some("AST grep".into()),
        description: Some("Run ast-grep. Provide rawArgs or pattern/paths/json.".into()),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}
async fn handle_astgrep(p: AstGrepParams) -> CallToolResult {
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(60000));
    let max_bytes = p.max_output_bytes.unwrap_or(120000);
    let mut cmd = Command::new("ast-grep");
    if let Some(args) = p.raw_args {
        for a in args {
            cmd.arg(a);
        }
    } else {
        if p.json.unwrap_or(true) {
            cmd.arg("--json");
        }
        if let Some(pt) = p.pattern {
            cmd.arg("-p").arg(pt);
        }
        if let Some(paths) = p.paths {
            for pth in paths {
                cmd.arg(pth);
            }
        } else {
            cmd.arg(".");
        }
    }
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => err("ast-grep timeout".into()),
        Ok(Err(e)) => err(format!("ast-grep spawn error: {e}")),
        Ok(Ok(out)) => {
            let mut buf = String::new();
            if !out.stdout.is_empty() {
                buf.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            if !out.stderr.is_empty() {
                buf.push_str("\n[stderr]\n");
                buf.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            let s = trunc_utf8(buf, max_bytes);
            if out.status.success() { ok(s) } else { err(s) }
        }
    }
}

// -------------------- chatgpt.applyPatch (git apply shortcut) --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchParams {
    pub patch: String,
}
fn tool_applypatch_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .into_generator()
        .into_root_schema_for::<ApplyPatchParams>();
    let input_schema = to_tool_input_schema("chatgpt.applyPatch", schema);
    Tool {
        name: "chatgpt.applyPatch".into(),
        title: Some("Apply patch".into()),
        description: Some("Apply a unified diff patch to the workspace.".into()),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}

async fn handle_applypatch(p: ApplyPatchParams) -> CallToolResult {
    let patch = p.patch;
    if patch.trim().is_empty() {
        return err("chatgpt.applyPatch requires a non-empty patch".into());
    }

    let join = task::spawn_blocking(move || {
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let outcome = run_apply_patch(&patch, &mut stdout_buf, &mut stderr_buf);
        (outcome, stdout_buf, stderr_buf)
    })
    .await;

    let (outcome, stdout_buf, stderr_buf) = match join {
        Ok(res) => res,
        Err(e) => {
            return err(format!("failed to apply patch (task join error): {e}"));
        }
    };

    let stdout_text = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    let stderr_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();

    match outcome {
        Ok(()) => {
            let mut parts = Vec::new();
            if !stdout_text.is_empty() {
                parts.push(stdout_text);
            }
            if !stderr_text.is_empty() {
                parts.push(stderr_text);
            }
            if parts.is_empty() {
                parts.push("Patch applied.".into());
            }
            ok(parts.join("\n"))
        }
        Err(e) => {
            let mut msg = format!("failed to apply patch: {e}");
            if !stderr_text.is_empty() {
                msg.push('\n');
                msg.push_str(&stderr_text);
            }
            err(msg)
        }
    }
}

// -------------------- chatgpt.exec --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecParams {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}
fn tool_exec_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .into_generator()
        .into_root_schema_for::<ExecParams>();
    let input_schema = to_tool_input_schema("chatgpt.exec", schema);
    Tool {
        name: "chatgpt.exec".into(),
        title: Some("Execute shell command".into()),
        description: Some("Run a shell command via bash -lc with optional cwd/timeout".into()),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}
async fn handle_exec(p: ExecParams) -> CallToolResult {
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(120_000));
    let max_bytes = p.max_output_bytes.unwrap_or(64 * 1024);
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(&p.command);
    if let Some(cwd) = p.cwd {
        cmd.current_dir(PathBuf::from(cwd));
    }
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => err(format!("timeout after {timeout:?}")),
        Ok(Err(e)) => err(format!("spawn error: {e}")),
        Ok(Ok(out)) => {
            let mut buf = String::new();
            if !out.stdout.is_empty() {
                buf.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            if !out.stderr.is_empty() {
                buf.push_str("\n[stderr]\n");
                buf.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            let code = out.status.code().unwrap_or(-1);
            let body = trunc_utf8(buf, max_bytes);
            if code == 0 {
                ok(body)
            } else {
                err(format!("exit {code}\n{body}"))
            }
        }
    }
}

// -------------------- chatgpt.ripgrep --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepParams {
    pub pattern: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_ripgrep_excludes")]
    pub globs_exclude: Vec<String>,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_ripgrep_excludes() -> Vec<String> {
    vec![".git".into(), "node_modules".into(), "target".into()]
}
fn tool_ripgrep_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .into_generator()
        .into_root_schema_for::<RipgrepParams>();
    let input_schema = to_tool_input_schema("chatgpt.ripgrep", schema);
    Tool {
        name: "chatgpt.ripgrep".into(),
        title: Some("Search with ripgrep".into()),
        description: Some("Run rg -n --json with excludes; returns JSON lines (truncated)".into()),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}
async fn handle_ripgrep(p: RipgrepParams) -> CallToolResult {
    let RipgrepParams {
        pattern,
        paths,
        globs_exclude,
        max_results,
        timeout_ms,
    } = p;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(60_000));
    let mut args: Vec<String> = vec![
        "-n".into(),
        "--json".into(),
        "--no-heading".into(),
        "--color".into(),
        "never".into(),
    ];
    let mut excludes = globs_exclude;
    for g in excludes.drain(..) {
        args.push("--glob".into());
        args.push(format!("!{g}"));
    }
    args.push("--".into());
    args.push(pattern);
    let mut cmd = Command::new("rg");
    for a in &args {
        cmd.arg(a);
    }
    if paths.is_empty() {
        cmd.arg(".");
    } else {
        for pth in paths {
            cmd.arg(pth);
        }
    }
    let max = max_results.unwrap_or(500);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => err(format!("ripgrep timeout after {timeout:?}")),
        Ok(Err(e)) => err(format!("ripgrep spawn error: {e}")),
        Ok(Ok(out)) => {
            if !out.status.success() {
                return err(String::from_utf8_lossy(&out.stderr).into());
            }
            let s = String::from_utf8_lossy(&out.stdout);
            let collected = s.lines().take(max).collect::<Vec<_>>().join("\n");
            ok(trunc_utf8(collected, 120_000))
        }
    }
}

// -------------------- chatgpt.readFile --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileParams {
    pub path: String,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}
fn tool_readfile_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .into_generator()
        .into_root_schema_for::<ReadFileParams>();
    let input_schema = to_tool_input_schema("chatgpt.readFile", schema);
    Tool {
        name: "chatgpt.readFile".into(),
        title: Some("Read file".into()),
        description: Some("Read a file with optional byte cap (UTF-8 safe)".into()),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}
async fn handle_readfile(p: ReadFileParams) -> CallToolResult {
    match tokio::fs::read(&p.path).await {
        Err(e) => err(format!("read error: {e}")),
        Ok(bytes) => {
            let max = p.max_bytes.unwrap_or(120_000);
            let s = String::from_utf8_lossy(&bytes).into_owned();
            ok(trunc_utf8(s, max))
        }
    }
}

// -------------------- registry --------------------
pub fn list_tools() -> Vec<Tool> {
    // Put README first so clients that just render the first items show the docs up front.
    vec![
        tool_readme_schema(),
        tool_toolhelp_schema(),
        tool_exec_schema(),
        tool_ripgrep_schema(),
        tool_readfile_schema(),
        tool_astgrep_schema(),
        tool_applypatch_schema(),
    ]
}

pub async fn dispatch(name: &str, args: Option<serde_json::Value>) -> CallToolResult {
    match name {
        "chatgpt.exec" => match args.and_then(|v| serde_json::from_value::<ExecParams>(v).ok()) {
            Some(p) => handle_exec(p).await,
            None => err("bad or missing args".into()),
        },
        "chatgpt.ripgrep" => {
            match args.and_then(|v| serde_json::from_value::<RipgrepParams>(v).ok()) {
                Some(p) => handle_ripgrep(p).await,
                None => err("bad or missing args".into()),
            }
        }
        "chatgpt.readFile" => {
            match args.and_then(|v| serde_json::from_value::<ReadFileParams>(v).ok()) {
                Some(p) => handle_readfile(p).await,
                None => err("bad or missing args".into()),
            }
        }
        "chatgpt.applyPatch" => {
            match args.and_then(|v| serde_json::from_value::<ApplyPatchParams>(v).ok()) {
                Some(p) => handle_applypatch(p).await,
                None => err("bad or missing args".into()),
            }
        }
        "chatgpt.astGrep" => {
            match args.and_then(|v| serde_json::from_value::<AstGrepParams>(v).ok()) {
                Some(p) => handle_astgrep(p).await,
                None => err("bad or missing args".into()),
            }
        }
        "chatgpt.README" => handle_readme_file().await,
        "chatgpt.toolHelp" => handle_toolhelp().await,
        other => err(format!("Unknown tool: {other}")),
    }
}

async fn handle_readme_file() -> CallToolResult {
    let txt: &str = include_str!("../../README.chatgpt-tools.md");
    ok(txt.to_string())
}

// -------------------- chatgpt.README --------------------
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadmeParams {
    #[serde(default)]
    pub verbose: Option<bool>,
}

fn tool_readme_schema() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .into_generator()
        .into_root_schema_for::<ReadmeParams>();
    let input_schema = to_tool_input_schema("chatgpt.README", schema);
    Tool {
        name: "chatgpt.README".into(),
        title: Some("Usage guide for chatgpt.* tools".into()),
        description: Some(
            "How-to and examples for exec, ripgrep, readFile, astGrep, applyPatch.".into(),
        ),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}

// -------------------- chatgpt.toolHelp (stub) --------------------
fn tool_toolhelp_schema() -> Tool {
    let input_schema = match serde_json::from_value(serde_json::json!({
        "type": "object", "properties": {}, "additionalProperties": false
    })) {
        Ok(schema) => schema,
        Err(e) => {
            error!("failed to build input schema for chatgpt.toolHelp: {e}");
            ToolInputSchema {
                r#type: "object".into(),
                properties: None,
                required: None,
            }
        }
    };
    Tool {
        name: "chatgpt.toolHelp".into(),
        title: Some("Tool help (use chatgpt.README)".into()),
        description: Some(
            "Deprecated. Call chatgpt.README for full documentation and examples.".into(),
        ),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}
async fn handle_toolhelp() -> CallToolResult {
    ok("Use chatgpt.README for detailed how-to of all chatgpt.* tools.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn astgrep_schema_omits_null_type_union() {
        let schema_json = match serde_json::to_value(tool_astgrep_schema().input_schema) {
            Ok(value) => value,
            Err(e) => panic!("failed to serialize astGrep schema: {e}"),
        };

        fn assert_no_null_union(value: &Value) {
            match value {
                Value::Object(obj) => {
                    if let Some(Value::Array(types)) = obj.get("type") {
                        let has_null = types.iter().any(|ty| match ty {
                            Value::String(s) => s == "null",
                            _ => false,
                        });
                        assert!(!has_null, "schema unexpectedly permits null via type union");
                    }
                    for child in obj.values() {
                        assert_no_null_union(child);
                    }
                }
                Value::Array(arr) => {
                    for child in arr {
                        assert_no_null_union(child);
                    }
                }
                _ => {}
            }
        }

        assert_no_null_union(&schema_json);
    }
}
