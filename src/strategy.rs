use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gray_matter::Matter;
use gray_matter::engine::YAML;
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use crate::config::{Config, OutputChannelConfig};
use crate::error::ConfigError;

// ── Embedded strategy files ────────────────────────────────────────────

const BASE_OPENCODE_JSON: &str = include_str!("strategies/opencode.json");

const BUILTIN_SIMPLE_PROMPT: &str = include_str!("strategies/simple/prompt.md");

const BUILTIN_AGENTIC_PROMPT: &str = include_str!("strategies/agentic/prompt.md");
const BUILTIN_AGENTIC_OPENCODE: &str = include_str!("strategies/agentic/opencode.json");

const BUILTIN_BRIEF_PROMPT: &str = include_str!("strategies/brief/prompt.md");

// ── Embedded tool files ────────────────────────────────────────────────

const TOOL_FETCH_ARTICLE: &str = include_str!("opencode_tools/fetch-article.ts");
const TOOL_PACKAGE_JSON: &str = include_str!("opencode_tools/package.json");

// ── Types ──────────────────────────────────────────────────────────────

/// Parsed YAML frontmatter from a strategy's `prompt.md`.
#[derive(Debug, Clone, Deserialize)]
pub struct StrategyFrontmatter {
    pub format_version: u32,
    pub name: String,
    pub description: String,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub tools: Vec<String>,
}

fn default_timeout() -> String {
    "30m".to_string()
}

fn default_max_retries() -> u32 {
    1
}

/// Where a strategy was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategySource {
    BuiltIn,
    User,
}

/// A fully loaded strategy.
#[derive(Debug, Clone)]
pub struct Strategy {
    pub meta: StrategyFrontmatter,
    pub prompt_body: String,
    pub opencode_overlay: Option<Value>,
    pub source: StrategySource,
    /// For user strategies: the directory on disk. None for built-ins.
    pub dir: Option<PathBuf>,
}

/// Registry of all available strategies (built-in + user-defined).
pub struct StrategyRegistry {
    strategies: HashMap<String, Strategy>,
}

impl StrategyRegistry {
    /// Load all built-in strategies plus any user-defined ones from `strategies_dir`.
    pub fn load(strategies_dir: Option<&Path>) -> Result<Self> {
        let mut strategies = HashMap::new();

        // Load built-ins
        let simple = load_builtin(BUILTIN_SIMPLE_PROMPT, None).context("loading built-in 'simple' strategy")?;
        strategies.insert("simple".to_string(), simple);

        let agentic = load_builtin(BUILTIN_AGENTIC_PROMPT, Some(BUILTIN_AGENTIC_OPENCODE))
            .context("loading built-in 'agentic' strategy")?;
        strategies.insert("agentic".to_string(), agentic);

        let brief = load_builtin(BUILTIN_BRIEF_PROMPT, None).context("loading built-in 'brief' strategy")?;
        strategies.insert("brief".to_string(), brief);

        // Load user strategies if configured
        if let Some(dir) = strategies_dir
            && dir.is_dir()
        {
            for entry in std::fs::read_dir(dir).with_context(|| format!("reading strategies_dir: {}", dir.display()))? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Skip shared_tools directory
                if path.file_name().is_some_and(|n| n == "shared_tools") {
                    continue;
                }
                // Only load dirs that contain a prompt.md
                if !path.join("prompt.md").exists() {
                    continue;
                }
                let strategy = load_user_strategy(&path)
                    .with_context(|| format!("loading user strategy from {}", path.display()))?;
                let name = strategy.meta.name.clone();
                if strategies.contains_key(&name) {
                    return Err(ConfigError::Validation(format!(
                        "user strategy '{name}' collides with built-in strategy name"
                    ))
                    .into());
                }
                strategies.insert(name, strategy);
            }
        }

        Ok(Self { strategies })
    }

    pub fn get(&self, name: &str) -> Option<&Strategy> {
        self.strategies.get(name)
    }

    pub fn list(&self) -> Vec<&Strategy> {
        let mut list: Vec<&Strategy> = self.strategies.values().collect();
        // Sort: built-ins first (alphabetically), then user (alphabetically)
        list.sort_by(|a, b| {
            a.source
                .cmp_builtin_first(&b.source)
                .then_with(|| a.meta.name.cmp(&b.meta.name))
        });
        list
    }
}

impl StrategySource {
    fn cmp_builtin_first(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (StrategySource::BuiltIn, StrategySource::User) => std::cmp::Ordering::Less,
            (StrategySource::User, StrategySource::BuiltIn) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    }
}

impl std::fmt::Display for StrategySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StrategySource::BuiltIn => write!(f, "built-in"),
            StrategySource::User => write!(f, "user"),
        }
    }
}

// ── Parsing ────────────────────────────────────────────────────────────

/// Parse a strategy prompt.md file into frontmatter and body.
fn parse_strategy_prompt(content: &str) -> Result<(StrategyFrontmatter, String)> {
    let matter = Matter::<YAML>::new();

    // Try typed deserialization first (happy path)
    if let Some(result) = matter.parse_with_struct::<StrategyFrontmatter>(content) {
        if result.data.format_version != 1 {
            anyhow::bail!(
                "unsupported strategy format_version {} (this binary supports version 1)",
                result.data.format_version
            );
        }
        return Ok((result.data, result.content));
    }

    // Typed parse failed — diagnose why for a useful error message
    let parsed = matter.parse(content);
    if parsed.data.is_none() {
        anyhow::bail!("strategy prompt.md must start with YAML frontmatter (---)");
    }

    // Frontmatter exists but doesn't match StrategyFrontmatter
    anyhow::bail!(
        "strategy frontmatter is valid YAML but doesn't match the expected schema \
         (required fields: format_version, name, description)"
    );
}

/// Load a built-in strategy from embedded strings.
fn load_builtin(prompt_content: &str, opencode_overlay: Option<&str>) -> Result<Strategy> {
    let (meta, prompt_body) = parse_strategy_prompt(prompt_content)?;

    let overlay = match opencode_overlay {
        Some(json_str) => Some(serde_json::from_str(json_str).context("parsing built-in opencode overlay")?),
        None => None,
    };

    Ok(Strategy {
        meta,
        prompt_body,
        opencode_overlay: overlay,
        source: StrategySource::BuiltIn,
        dir: None,
    })
}

/// Load a user strategy from a directory on disk.
pub fn load_user_strategy(dir: &Path) -> Result<Strategy> {
    let prompt_path = dir.join("prompt.md");
    let prompt_content =
        std::fs::read_to_string(&prompt_path).with_context(|| format!("reading {}", prompt_path.display()))?;

    let (meta, prompt_body) = parse_strategy_prompt(&prompt_content)?;

    let opencode_path = dir.join("opencode.json");
    let overlay = if opencode_path.exists() {
        let json_str =
            std::fs::read_to_string(&opencode_path).with_context(|| format!("reading {}", opencode_path.display()))?;
        Some(serde_json::from_str(&json_str).with_context(|| format!("parsing {}", opencode_path.display()))?)
    } else {
        None
    };

    Ok(Strategy {
        meta,
        prompt_body,
        opencode_overlay: overlay,
        source: StrategySource::User,
        dir: Some(dir.to_path_buf()),
    })
}

// ── Deep merge ─────────────────────────────────────────────────────────

/// Deep-merge two JSON values. Overlay wins on conflicts.
/// Objects merge recursively. Arrays replaced wholesale. Null in overlay deletes the key.
pub fn deep_merge(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            let mut result = base_map.clone();
            for (key, overlay_val) in overlay_map {
                if overlay_val.is_null() {
                    // Null in overlay = delete
                    result.remove(key);
                } else if let Some(base_val) = base_map.get(key) {
                    result.insert(key.clone(), deep_merge(base_val, overlay_val));
                } else {
                    result.insert(key.clone(), overlay_val.clone());
                }
            }
            Value::Object(result)
        }
        // Non-object: overlay wins
        (_, overlay) => overlay.clone(),
    }
}

/// Compute the final opencode.json by merging global base + strategy overlay.
pub fn resolve_opencode_config(strategy: &Strategy) -> Result<Value> {
    let base: Value = serde_json::from_str(BASE_OPENCODE_JSON).context("parsing base opencode.json")?;

    match &strategy.opencode_overlay {
        Some(overlay) => Ok(deep_merge(&base, overlay)),
        None => Ok(base),
    }
}

// ── Tool resolution ────────────────────────────────────────────────────

/// Known built-in tools mapped by name.
struct BuiltinTool {
    files: Vec<(&'static str, &'static str)>,
    package_json: Option<&'static str>,
}

fn builtin_tools() -> HashMap<&'static str, BuiltinTool> {
    let mut map = HashMap::new();
    map.insert(
        "fetch-article",
        BuiltinTool {
            files: vec![("fetch-article.ts", TOOL_FETCH_ARTICLE)],
            package_json: Some(TOOL_PACKAGE_JSON),
        },
    );
    map
}

/// Resolved tool files to write to the workspace.
pub struct ResolvedTools {
    /// Tool files: (relative path under .opencode/tools/, content)
    pub tool_files: Vec<(String, String)>,
    /// Merged package.json for all tools
    pub package_json: Value,
}

/// Resolve tools from a strategy's frontmatter. Returns files + merged package.json.
pub fn resolve_tools(strategy: &Strategy) -> Result<ResolvedTools> {
    let builtins = builtin_tools();
    let mut tool_files = Vec::new();
    let mut merged_deps: serde_json::Map<String, Value> = serde_json::Map::new();

    for tool_name in &strategy.meta.tools {
        if tool_name.starts_with("./") || tool_name.starts_with("../") {
            // User tool: relative path from strategy dir
            let strategy_dir = strategy.dir.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "strategy '{}' references user tool '{}' but has no directory (built-in strategies can only use built-in tools)",
                    strategy.meta.name, tool_name
                )
            })?;

            let tool_path = strategy_dir.join(tool_name);
            let content = std::fs::read_to_string(&tool_path)
                .with_context(|| format!("reading user tool: {}", tool_path.display()))?;

            let filename = tool_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("tool path has no filename: {tool_name}"))?
                .to_string_lossy()
                .to_string();

            tool_files.push((filename, content));

            // Check for user tool's package.json in the same directory
            let tool_dir = tool_path.parent().unwrap_or(strategy_dir);
            let pkg_path = tool_dir.join("package.json");
            if pkg_path.exists() {
                let pkg_content =
                    std::fs::read_to_string(&pkg_path).with_context(|| format!("reading {}", pkg_path.display()))?;
                let pkg: Value =
                    serde_json::from_str(&pkg_content).with_context(|| format!("parsing {}", pkg_path.display()))?;
                if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_object()) {
                    for (k, v) in deps {
                        merged_deps.insert(k.clone(), v.clone());
                    }
                }
            }
        } else {
            // Built-in tool
            let builtin = builtins.get(tool_name.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "strategy '{}' references unknown built-in tool '{}'",
                    strategy.meta.name,
                    tool_name
                )
            })?;

            for (filename, content) in &builtin.files {
                tool_files.push((filename.to_string(), content.to_string()));
            }

            if let Some(pkg_str) = builtin.package_json {
                let pkg: Value = serde_json::from_str(pkg_str).context("parsing built-in package.json")?;
                if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_object()) {
                    for (k, v) in deps {
                        merged_deps.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }

    let package_json = serde_json::json!({ "dependencies": merged_deps });

    Ok(ResolvedTools {
        tool_files,
        package_json,
    })
}

// ── Workspace context ──────────────────────────────────────────────────

/// Returns the `## Workspace` section describing the workspace file layout.
/// Dynamically lists tools based on the strategy's tool list.
/// When `include_output_md` is true, includes the `output.md` bullet (for generation mode).
pub fn workspace_context(strategy: &Strategy, include_output_md: bool) -> String {
    let mut ctx = String::from(
        "\n## Workspace\n\
         All input data is in the current directory:\n\
         - `manifest.json` — generation metadata (channel config, time window, source list)\n\
         - `sources/` — one markdown file per source (`<slug>.md`), each with a YAML frontmatter\n\
         \x20 header (name, type, item_count, description) followed by content items separated by `---`\n",
    );

    // List tools dynamically
    for tool_name in &strategy.meta.tools {
        if tool_name == "fetch-article" {
            ctx.push_str(
                "- `.opencode/tools/fetch-article.ts` — custom tool for clean article extraction \
                 (uses Readability for token-efficient content)\n",
            );
        } else if tool_name.starts_with("./") || tool_name.starts_with("../") {
            // User tool — just show the filename
            if let Some(filename) = Path::new(tool_name).file_name() {
                ctx.push_str(&format!(
                    "- `.opencode/tools/{}` — custom tool\n",
                    filename.to_string_lossy()
                ));
            }
        } else {
            ctx.push_str(&format!("- `.opencode/tools/{tool_name}` — custom tool\n"));
        }
    }

    if include_output_md {
        ctx.push_str("- `output.md` — write the final article HERE\n");
    }
    ctx
}

// ── Strategy resolution ────────────────────────────────────────────────

/// Resolve the strategy name for a channel: channel override → global default → "simple".
pub fn resolve_strategy_name(config: &Config, channel_config: &OutputChannelConfig) -> String {
    channel_config
        .strategy
        .clone()
        .unwrap_or_else(|| config.pail.default_strategy.clone())
}

// ── Validation ─────────────────────────────────────────────────────────

/// Validate that all referenced strategy names exist in the registry.
pub fn validate_strategy_config(config: &Config, registry: &StrategyRegistry) -> Result<()> {
    // Check default strategy
    let default = &config.pail.default_strategy;
    if registry.get(default).is_none() {
        return Err(ConfigError::Validation(format!(
            "[pail].default_strategy '{default}' does not match any known strategy"
        ))
        .into());
    }

    // Check per-channel strategies
    for channel in &config.output_channel {
        if let Some(ref strategy_name) = channel.strategy
            && registry.get(strategy_name).is_none()
        {
            return Err(ConfigError::Validation(format!(
                "output channel '{}': strategy '{strategy_name}' does not match any known strategy",
                channel.name
            ))
            .into());
        }
    }

    // Validate strategy prompts contain {editorial_directive}
    for strategy in registry.list() {
        if !strategy.prompt_body.contains("{editorial_directive}") {
            warn!(
                strategy = %strategy.meta.name,
                "strategy prompt does not contain {{editorial_directive}} placeholder"
            );
        }

        // Validate timeout is parseable
        humantime::parse_duration(&strategy.meta.timeout).map_err(|e| {
            ConfigError::Validation(format!(
                "strategy '{}': invalid timeout '{}': {}",
                strategy.meta.name, strategy.meta.timeout, e
            ))
        })?;

        // Validate tools resolve
        resolve_tools(strategy).with_context(|| format!("validating tools for strategy '{}'", strategy.meta.name))?;
    }

    Ok(())
}
