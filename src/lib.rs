use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SNAPSHOT_VERSION: u32 = 3;
pub const CONFIG_VERSION: u32 = 1;
pub const CATEGORY_CONFIG_VERSION: u32 = 1;
pub const WORKSPACE_REGISTRY_VERSION: u32 = 1;
pub const CLEANUP_JOURNAL_VERSION: u32 = 1;
pub const CACHE_USAGE_JOURNAL_VERSION: u32 = 1;
pub const AUDIT_REPORT_VERSION: u32 = 1;
const DEFAULT_STALE_DAYS: u64 = 15;
const DEFAULT_TOP_LIMIT: usize = 1_000;
const DEFAULT_AUDIT_TOP_LIMIT: usize = 10_000;
const APP_DIR_NAME: &str = "dscan11";
const WORKSPACES_DIR_NAME: &str = "workspaces";
const WORKSPACE_REGISTRY_FILE: &str = "workspaces.json";
pub const DEFAULT_WORKSPACE_NAME: &str = "default";

#[derive(Debug)]
pub enum CliError {
    Io {
        context: String,
        source: io::Error,
    },
    Json {
        context: String,
        source: serde_json::Error,
    },
    Message(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Message(_) => 2,
            CliError::Io { .. } | CliError::Json { .. } => 1,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Io { context, source } => write!(f, "{context}: {source}"),
            CliError::Json { context, source } => write!(f, "{context}: {source}"),
            CliError::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CliError {}

fn config_version() -> u32 {
    CONFIG_VERSION
}

fn category_config_version() -> u32 {
    CATEGORY_CONFIG_VERSION
}

fn workspace_registry_version() -> u32 {
    WORKSPACE_REGISTRY_VERSION
}

fn cleanup_journal_version() -> u32 {
    CLEANUP_JOURNAL_VERSION
}

fn cache_usage_journal_version() -> u32 {
    CACHE_USAGE_JOURNAL_VERSION
}

fn deserialize_config_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_supported_version(deserializer, "config", CONFIG_VERSION)
}

fn deserialize_category_config_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_supported_version(deserializer, "category config", CATEGORY_CONFIG_VERSION)
}

fn deserialize_workspace_registry_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_supported_version(
        deserializer,
        "workspace registry",
        WORKSPACE_REGISTRY_VERSION,
    )
}

fn deserialize_cleanup_journal_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_supported_version(deserializer, "cleanup journal", CLEANUP_JOURNAL_VERSION)
}

fn deserialize_cache_usage_journal_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_supported_version(
        deserializer,
        "cache usage journal",
        CACHE_USAGE_JOURNAL_VERSION,
    )
}

fn deserialize_supported_version<'de, D>(
    deserializer: D,
    label: &str,
    supported: u32,
) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version > supported {
        return Err(serde::de::Error::custom(format!(
            "unsupported {label} version {version}; this dscan11 supports {supported}"
        )));
    }
    Ok(version)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

impl OutputMode {
    pub fn from_json(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }

    pub fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

#[derive(Debug, Clone)]
pub struct CachePaths {
    pub app_dir: PathBuf,
    pub base_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub workspace_registry_path: PathBuf,
    pub workspace_name: String,
    pub config_path: PathBuf,
    pub category_config_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub base_snapshot_path: PathBuf,
    pub cleanup_journal_path: PathBuf,
    pub cache_usage_journal_path: PathBuf,
}

pub fn cache_paths() -> Result<CachePaths, CliError> {
    cache_paths_for_workspace(None)
}

pub fn cache_paths_for_workspace(workspace: Option<&str>) -> Result<CachePaths, CliError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("AppData").join("Local"))
        })
        .ok_or_else(|| {
            CliError::Message("unable to locate LOCALAPPDATA or USERPROFILE".to_string())
        })?;

    let app_dir = base.join(APP_DIR_NAME);
    migrate_legacy_cache(&app_dir)?;
    let mut registry = load_or_init_workspace_registry(&app_dir)?;
    let workspace_name = match workspace {
        Some(name) => {
            validate_workspace_name(name)?;
            if !registry.workspaces.contains_key(name) {
                return Err(CliError::Message(format!(
                    "workspace `{name}` does not exist; run `dscan11 workspace create {name}` first"
                )));
            }
            name.to_string()
        }
        None => registry.active.clone(),
    };
    validate_workspace_name(&workspace_name)?;
    if !registry.workspaces.contains_key(&workspace_name) {
        registry
            .workspaces
            .insert(workspace_name.clone(), WorkspaceInfo::new()?);
        save_workspace_registry(&app_dir, &registry)?;
    }

    workspace_cache_paths(&app_dir, &workspace_name)
}

fn workspace_cache_paths(app_dir: &Path, workspace_name: &str) -> Result<CachePaths, CliError> {
    validate_workspace_name(workspace_name)?;
    let workspaces_dir = app_dir.join(WORKSPACES_DIR_NAME);
    let base_dir = workspaces_dir.join(workspace_name);
    Ok(CachePaths {
        config_path: app_dir.join("config.json"),
        category_config_path: app_dir.join("categories.json"),
        snapshot_path: base_dir.join("snapshot.json"),
        base_snapshot_path: base_dir.join("base-snapshot.json"),
        cleanup_journal_path: base_dir.join("cleanup-journal.jsonl"),
        cache_usage_journal_path: base_dir.join("cache-usage-journal.jsonl"),
        workspace_registry_path: app_dir.join(WORKSPACE_REGISTRY_FILE),
        workspace_name: workspace_name.to_string(),
        workspaces_dir,
        app_dir: app_dir.to_path_buf(),
        base_dir,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRegistry {
    #[serde(
        default = "workspace_registry_version",
        deserialize_with = "deserialize_workspace_registry_version"
    )]
    pub version: u32,
    pub active: String,
    pub workspaces: BTreeMap<String, WorkspaceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub created_at_unix: u64,
}

impl WorkspaceInfo {
    fn new() -> Result<Self, CliError> {
        Ok(Self {
            created_at_unix: current_unix()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceView {
    pub name: String,
    pub active: bool,
    pub cache_dir: String,
    pub has_snapshot: bool,
    pub created_at_unix: u64,
}

fn workspace_registry_path(app_dir: &Path) -> PathBuf {
    app_dir.join(WORKSPACE_REGISTRY_FILE)
}

fn load_or_init_workspace_registry(app_dir: &Path) -> Result<WorkspaceRegistry, CliError> {
    match fs::read_to_string(workspace_registry_path(app_dir)) {
        Ok(contents) => {
            let registry: WorkspaceRegistry =
                serde_json::from_str(&contents).map_err(|source| CliError::Json {
                    context: format!(
                        "failed to parse workspace registry {}",
                        workspace_registry_path(app_dir).display()
                    ),
                    source,
                })?;
            validate_workspace_name(&registry.active)?;
            for name in registry.workspaces.keys() {
                validate_workspace_name(name)?;
            }
            Ok(registry)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let mut workspaces = BTreeMap::new();
            workspaces.insert(DEFAULT_WORKSPACE_NAME.to_string(), WorkspaceInfo::new()?);
            let registry = WorkspaceRegistry {
                version: WORKSPACE_REGISTRY_VERSION,
                active: DEFAULT_WORKSPACE_NAME.to_string(),
                workspaces,
            };
            save_workspace_registry(app_dir, &registry)?;
            Ok(registry)
        }
        Err(source) => Err(CliError::Io {
            context: format!(
                "failed to read workspace registry {}",
                workspace_registry_path(app_dir).display()
            ),
            source,
        }),
    }
}

fn save_workspace_registry(app_dir: &Path, registry: &WorkspaceRegistry) -> Result<(), CliError> {
    fs::create_dir_all(app_dir).map_err(|source| CliError::Io {
        context: format!("failed to create cache directory {}", app_dir.display()),
        source,
    })?;
    let json = serde_json::to_string_pretty(registry).map_err(|source| CliError::Json {
        context: "failed to serialize workspace registry".to_string(),
        source,
    })?;
    fs::write(workspace_registry_path(app_dir), json).map_err(|source| CliError::Io {
        context: format!(
            "failed to write workspace registry {}",
            workspace_registry_path(app_dir).display()
        ),
        source,
    })
}

fn migrate_legacy_cache(app_dir: &Path) -> Result<(), CliError> {
    if workspace_registry_path(app_dir).exists() {
        return Ok(());
    }

    let legacy_files = [
        "snapshot.json",
        "base-snapshot.json",
        "cleanup-journal.jsonl",
        "cache-usage-journal.jsonl",
    ];
    let has_legacy_cache = legacy_files.iter().any(|name| app_dir.join(name).exists());
    if has_legacy_cache {
        let default_dir = app_dir
            .join(WORKSPACES_DIR_NAME)
            .join(DEFAULT_WORKSPACE_NAME);
        fs::create_dir_all(&default_dir).map_err(|source| CliError::Io {
            context: format!(
                "failed to create default workspace directory {}",
                default_dir.display()
            ),
            source,
        })?;
        for file_name in legacy_files {
            let from = app_dir.join(file_name);
            if from.exists() {
                let to = default_dir.join(file_name);
                if to.exists() {
                    return Err(CliError::Message(format!(
                        "cannot migrate legacy cache because {} already exists",
                        to.display()
                    )));
                }
                fs::rename(&from, &to).map_err(|source| CliError::Io {
                    context: format!("failed to migrate {} to {}", from.display(), to.display()),
                    source,
                })?;
            }
        }
    }

    let mut workspaces = BTreeMap::new();
    workspaces.insert(DEFAULT_WORKSPACE_NAME.to_string(), WorkspaceInfo::new()?);
    save_workspace_registry(
        app_dir,
        &WorkspaceRegistry {
            version: WORKSPACE_REGISTRY_VERSION,
            active: DEFAULT_WORKSPACE_NAME.to_string(),
            workspaces,
        },
    )
}

pub fn validate_workspace_name(name: &str) -> Result<(), CliError> {
    if name.is_empty() {
        return Err(CliError::Message(
            "invalid workspace name \"\"; use letters, numbers, dots, dashes, or underscores only"
                .to_string(),
        ));
    }
    if name == "." || name == ".." {
        return Err(CliError::Message(format!(
            "invalid workspace name \"{name}\"; workspace names cannot be \".\" or \"..\""
        )));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(CliError::Message(format!(
            "invalid workspace name \"{name}\"; use letters, numbers, dots, dashes, or underscores only"
        )));
    }
    Ok(())
}

pub fn workspace_exists(paths: &CachePaths, name: &str) -> Result<bool, CliError> {
    validate_workspace_name(name)?;
    let registry = load_or_init_workspace_registry(&paths.app_dir)?;
    Ok(registry.workspaces.contains_key(name))
}

pub fn create_workspace(paths: &CachePaths, name: &str) -> Result<WorkspaceView, CliError> {
    validate_workspace_name(name)?;
    let mut registry = load_or_init_workspace_registry(&paths.app_dir)?;
    if registry.workspaces.contains_key(name) {
        return Err(CliError::Message(format!(
            "workspace `{name}` already exists; run `dscan11 workspace use {name}` to select it"
        )));
    }
    registry
        .workspaces
        .insert(name.to_string(), WorkspaceInfo::new()?);
    let created_paths = workspace_cache_paths(&paths.app_dir, name)?;
    fs::create_dir_all(&created_paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create workspace directory {}",
            created_paths.base_dir.display()
        ),
        source,
    })?;
    save_workspace_registry(&paths.app_dir, &registry)?;
    workspace_view(
        &paths.app_dir,
        name,
        false,
        registry.workspaces.get(name).expect("created"),
    )
}

pub fn use_workspace(paths: &CachePaths, name: &str) -> Result<WorkspaceView, CliError> {
    validate_workspace_name(name)?;
    let mut registry = load_or_init_workspace_registry(&paths.app_dir)?;
    let Some(info) = registry.workspaces.get(name).cloned() else {
        return Err(CliError::Message(format!(
            "workspace `{name}` does not exist; run `dscan11 workspace create {name}` first"
        )));
    };
    registry.active = name.to_string();
    save_workspace_registry(&paths.app_dir, &registry)?;
    workspace_view(&paths.app_dir, name, true, &info)
}

pub fn rename_workspace(
    paths: &CachePaths,
    old_name: &str,
    new_name: &str,
) -> Result<(), CliError> {
    validate_workspace_name(old_name)?;
    validate_workspace_name(new_name)?;
    if old_name == new_name {
        return Err(CliError::Message(format!(
            "workspace `{old_name}` is already named `{new_name}`"
        )));
    }
    let mut registry = load_or_init_workspace_registry(&paths.app_dir)?;
    let Some(info) = registry.workspaces.remove(old_name) else {
        return Err(CliError::Message(format!(
            "workspace `{old_name}` does not exist"
        )));
    };
    if registry.workspaces.contains_key(new_name) {
        registry.workspaces.insert(old_name.to_string(), info);
        return Err(CliError::Message(format!(
            "workspace `{new_name}` already exists"
        )));
    }
    let old_paths = workspace_cache_paths(&paths.app_dir, old_name)?;
    let new_paths = workspace_cache_paths(&paths.app_dir, new_name)?;
    if new_paths.base_dir.exists() {
        registry.workspaces.insert(old_name.to_string(), info);
        return Err(CliError::Message(format!(
            "workspace directory already exists: {}",
            new_paths.base_dir.display()
        )));
    }
    if old_paths.base_dir.exists() {
        fs::rename(&old_paths.base_dir, &new_paths.base_dir).map_err(|source| CliError::Io {
            context: format!(
                "failed to rename workspace directory {} to {}",
                old_paths.base_dir.display(),
                new_paths.base_dir.display()
            ),
            source,
        })?;
    }
    registry.workspaces.insert(new_name.to_string(), info);
    if registry.active == old_name {
        registry.active = new_name.to_string();
    }
    save_workspace_registry(&paths.app_dir, &registry)
}

pub fn delete_workspace(paths: &CachePaths, name: &str, force: bool) -> Result<(), CliError> {
    validate_workspace_name(name)?;
    let mut registry = load_or_init_workspace_registry(&paths.app_dir)?;
    if registry.active == name {
        return Err(CliError::Message(format!(
            "cannot delete active workspace `{name}`; switch workspaces first"
        )));
    }
    if registry.workspaces.remove(name).is_none() {
        return Err(CliError::Message(format!(
            "workspace `{name}` does not exist"
        )));
    }
    let target_paths = workspace_cache_paths(&paths.app_dir, name)?;
    if target_paths.base_dir.exists() {
        if !force {
            return Err(CliError::Message(format!(
                "workspace `{name}` has cache files; rerun with `--force` to delete it"
            )));
        }
        fs::remove_dir_all(&target_paths.base_dir).map_err(|source| CliError::Io {
            context: format!(
                "failed to delete workspace directory {}",
                target_paths.base_dir.display()
            ),
            source,
        })?;
    }
    save_workspace_registry(&paths.app_dir, &registry)
}

pub fn list_workspaces(paths: &CachePaths) -> Result<Vec<WorkspaceView>, CliError> {
    let registry = load_or_init_workspace_registry(&paths.app_dir)?;
    registry
        .workspaces
        .iter()
        .map(|(name, info)| workspace_view(&paths.app_dir, name, registry.active == *name, info))
        .collect()
}

pub fn current_workspace(paths: &CachePaths) -> Result<WorkspaceView, CliError> {
    let registry = load_or_init_workspace_registry(&paths.app_dir)?;
    let Some(info) = registry.workspaces.get(&registry.active) else {
        return Err(CliError::Message(format!(
            "active workspace `{}` is missing from the workspace registry",
            registry.active
        )));
    };
    workspace_view(&paths.app_dir, &registry.active, true, info)
}

fn workspace_view(
    app_dir: &Path,
    name: &str,
    active: bool,
    info: &WorkspaceInfo,
) -> Result<WorkspaceView, CliError> {
    let paths = workspace_cache_paths(app_dir, name)?;
    Ok(WorkspaceView {
        name: name.to_string(),
        active,
        cache_dir: paths.base_dir.display().to_string(),
        has_snapshot: paths.snapshot_path.exists(),
        created_at_unix: info.created_at_unix,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    #[serde(
        default = "config_version",
        deserialize_with = "deserialize_config_version"
    )]
    pub version: u32,
    pub stale_days: u64,
    pub top_limit: usize,
    pub skip_names: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            stale_days: DEFAULT_STALE_DAYS,
            top_limit: DEFAULT_TOP_LIMIT,
            skip_names: vec![
                "System Volume Information".to_string(),
                "$Recycle.Bin".to_string(),
                "Windows".to_string(),
                "Recovery".to_string(),
                "Config.Msi".to_string(),
                "$WinREAgent".to_string(),
            ],
        }
    }
}

pub fn load_or_default_config(paths: &CachePaths) -> Result<AppConfig, CliError> {
    match fs::read_to_string(&paths.config_path) {
        Ok(contents) => serde_json::from_str(&contents).map_err(|source| CliError::Json {
            context: format!("failed to parse config {}", paths.config_path.display()),
            source,
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(source) => Err(CliError::Io {
            context: format!("failed to read config {}", paths.config_path.display()),
            source,
        }),
    }
}

pub fn save_config(paths: &CachePaths, config: &AppConfig) -> Result<(), CliError> {
    fs::create_dir_all(&paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create cache directory {}",
            paths.base_dir.display()
        ),
        source,
    })?;
    let json = serde_json::to_string_pretty(config).map_err(|source| CliError::Json {
        context: "failed to serialize config".to_string(),
        source,
    })?;
    fs::write(&paths.config_path, json).map_err(|source| CliError::Io {
        context: format!("failed to write config {}", paths.config_path.display()),
        source,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryConfigBootstrap {
    pub created: bool,
    pub category_config_path: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryConfig {
    #[serde(
        default = "category_config_version",
        deserialize_with = "deserialize_category_config_version"
    )]
    pub version: u32,
    pub categories: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub path_rules: Option<BTreeMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRules {
    path_rules: Vec<PathRule>,
    extension_map: HashMap<String, String>,
    fingerprint: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathRule {
    fragment: String,
    category: String,
}

impl CategoryRules {
    pub fn classify(&self, path: &Path) -> String {
        let normalized_path = normalize_path_fragment(&path.display().to_string());
        if let Some(rule) = self
            .path_rules
            .iter()
            .find(|rule| normalized_path.contains(&rule.fragment))
        {
            return rule.category.clone();
        }

        let ext = path
            .extension()
            .and_then(OsStr::to_str)
            .map(normalize_extension)
            .unwrap_or_default();

        if let Some(category) = self.extension_map.get(&ext) {
            return category.clone();
        }

        if has_component(path, "node_modules")
            || has_component(path, "target")
            || has_component(path, ".git")
        {
            return "Developer / Code".to_string();
        }

        if has_component(path, "Temp")
            || has_component(path, "Cache")
            || has_component(path, ".cache")
        {
            return "Temporary / Cache".to_string();
        }

        if has_component(path, "OneDrive") {
            return "Cloud / OneDrive".to_string();
        }

        "Other".to_string()
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

impl Default for CategoryConfig {
    fn default() -> Self {
        let mut categories = BTreeMap::new();
        for (name, extensions) in default_category_rules() {
            categories.insert(
                name.to_string(),
                extensions.iter().map(|ext| ext.to_string()).collect(),
            );
        }
        Self {
            version: CATEGORY_CONFIG_VERSION,
            categories,
            path_rules: Some(default_path_rules_config()),
        }
    }
}

pub fn load_category_rules(paths: &CachePaths) -> Result<CategoryRules, CliError> {
    match fs::read_to_string(&paths.category_config_path) {
        Ok(contents) => {
            let config: CategoryConfig =
                serde_json::from_str(&contents).map_err(|source| CliError::Json {
                    context: format!(
                        "failed to parse category config {}",
                        paths.category_config_path.display()
                    ),
                    source,
                })?;
            CategoryRules::from_config(config, paths.category_config_path.display().to_string())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            CategoryRules::from_config(CategoryConfig::default(), "builtin".to_string())
        }
        Err(source) => Err(CliError::Io {
            context: format!(
                "failed to read category config {}",
                paths.category_config_path.display()
            ),
            source,
        }),
    }
}

pub fn init_category_config(paths: &CachePaths) -> Result<CategoryConfigBootstrap, CliError> {
    if paths.category_config_path.exists() {
        return Ok(CategoryConfigBootstrap {
            created: false,
            category_config_path: paths.category_config_path.display().to_string(),
            source: "existing".to_string(),
        });
    }

    fs::create_dir_all(&paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create cache directory {}",
            paths.base_dir.display()
        ),
        source,
    })?;
    let json = serde_json::to_string_pretty(&CategoryConfig::default()).map_err(|source| {
        CliError::Json {
            context: "failed to serialize default category config".to_string(),
            source,
        }
    })?;
    fs::write(&paths.category_config_path, json).map_err(|source| CliError::Io {
        context: format!(
            "failed to write category config {}",
            paths.category_config_path.display()
        ),
        source,
    })?;

    Ok(CategoryConfigBootstrap {
        created: true,
        category_config_path: paths.category_config_path.display().to_string(),
        source: "defaults".to_string(),
    })
}

impl CategoryRules {
    pub fn from_config(config: CategoryConfig, source: String) -> Result<Self, CliError> {
        let canonical = normalize_category_config(config)?;
        let mut extension_map = HashMap::new();
        for (category, extensions) in &canonical.categories {
            for extension in extensions {
                extension_map.insert(extension.clone(), category.clone());
            }
        }
        let mut path_rules = Vec::new();
        for (category, fragments) in &canonical.path_rules {
            for fragment in fragments {
                path_rules.push(PathRule {
                    fragment: fragment.clone(),
                    category: category.clone(),
                });
            }
        }

        Ok(Self {
            path_rules,
            extension_map,
            fingerprint: category_fingerprint(&canonical),
            source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedCategoryConfig {
    categories: BTreeMap<String, Vec<String>>,
    path_rules: BTreeMap<String, Vec<String>>,
}

fn normalize_category_config(config: CategoryConfig) -> Result<NormalizedCategoryConfig, CliError> {
    let mut extension_owner = HashMap::new();
    let mut canonical_categories = BTreeMap::new();

    for (category, extensions) in config.categories {
        let category = category.trim().to_string();
        if category.is_empty() {
            return Err(CliError::Message(
                "category config contains an empty category name".to_string(),
            ));
        }

        let mut normalized_extensions = extensions
            .into_iter()
            .map(|extension| normalize_extension(&extension))
            .filter(|extension| !extension.is_empty())
            .collect::<Vec<_>>();
        normalized_extensions.sort();
        normalized_extensions.dedup();

        for extension in &normalized_extensions {
            if let Some(previous) = extension_owner.insert(extension.clone(), category.clone()) {
                return Err(CliError::Message(format!(
                    "category config assigns .{extension} to both {previous} and {category}"
                )));
            }
        }

        canonical_categories.insert(category, normalized_extensions);
    }

    let mut path_owner = HashMap::new();
    let mut canonical_path_rules = BTreeMap::new();
    for (category, fragments) in config.path_rules.unwrap_or_else(default_path_rules_config) {
        let category = category.trim().to_string();
        if category.is_empty() {
            return Err(CliError::Message(
                "category config contains an empty path rule category name".to_string(),
            ));
        }

        let mut normalized_fragments = fragments
            .into_iter()
            .map(|fragment| normalize_path_fragment(&fragment))
            .filter(|fragment| !fragment.is_empty())
            .collect::<Vec<_>>();
        normalized_fragments.sort();
        normalized_fragments.dedup();

        for fragment in &normalized_fragments {
            if let Some(previous) = path_owner.insert(fragment.clone(), category.clone()) {
                return Err(CliError::Message(format!(
                    "category config assigns path rule {fragment:?} to both {previous} and {category}"
                )));
            }
        }

        canonical_path_rules.insert(category, normalized_fragments);
    }

    Ok(NormalizedCategoryConfig {
        categories: canonical_categories,
        path_rules: canonical_path_rules,
    })
}

fn normalize_extension(extension: &str) -> String {
    extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
}

fn normalize_path_fragment(fragment: &str) -> String {
    fragment
        .trim()
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_lowercase()
}

fn category_fingerprint(canonical: &NormalizedCategoryConfig) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    fn feed(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    }

    feed(&mut hash, b"dscan11-category-rules-v2\nextensions\n");
    for (category, extensions) in &canonical.categories {
        feed(&mut hash, category.as_bytes());
        feed(&mut hash, b"\0");
        for extension in extensions {
            feed(&mut hash, extension.as_bytes());
            feed(&mut hash, b"\0");
        }
        feed(&mut hash, b"\n");
    }
    feed(&mut hash, b"path-rules\n");
    for (category, fragments) in &canonical.path_rules {
        feed(&mut hash, category.as_bytes());
        feed(&mut hash, b"\0");
        for fragment in fragments {
            feed(&mut hash, fragment.as_bytes());
            feed(&mut hash, b"\0");
        }
        feed(&mut hash, b"\n");
    }
    format!("fnv1a64:{hash:016x}")
}

fn default_category_rules() -> [(&'static str, &'static [&'static str]); 9] {
    [
        (
            "Apps / Executables",
            &["exe", "msi", "msix", "appx", "dll", "sys"],
        ),
        (
            "Documents",
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "csv",
                "odt", "ods", "odp",
            ],
        ),
        (
            "Pictures",
            &[
                "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "raw", "svg",
                "ico",
            ],
        ),
        (
            "Videos",
            &["mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v"],
        ),
        (
            "Music / Audio",
            &["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma"],
        ),
        (
            "Archives",
            &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"],
        ),
        (
            "Disk Images / VMs",
            &["iso", "img", "vhd", "vhdx", "vmdk", "ova", "ovf", "qcow2"],
        ),
        (
            "Developer / Code",
            &[
                "rs", "py", "js", "ts", "tsx", "jsx", "java", "cs", "cpp", "c", "h", "hpp", "go",
                "rb", "php", "html", "css", "json", "toml", "yaml", "yml", "xml", "sql", "lock",
            ],
        ),
        ("Temporary / Cache", &["tmp", "temp", "log", "bak", "dmp"]),
    ]
}

fn default_path_rules_config() -> BTreeMap<String, Vec<String>> {
    let mut path_rules = BTreeMap::new();
    for (name, fragments) in default_path_rules() {
        path_rules.insert(
            name.to_string(),
            fragments
                .iter()
                .map(|fragment| fragment.to_string())
                .collect(),
        );
    }
    path_rules
}

fn default_path_rules() -> [(&'static str, &'static [&'static str]); 2] {
    [
        (
            "AI Models",
            &[
                ".ollama/models",
                ".cache/huggingface/hub",
                "huggingface/hub/models--",
                "LM Studio/models",
                "GPT4All",
            ],
        ),
        (
            "Docker / Containers",
            &[
                "AppData/Local/Docker/wsl",
                "AppData/Local/Docker Desktop",
                "ProgramData/Docker",
                "ProgramData/docker/windowsfilter",
                "ProgramData/docker/containers",
                "ProgramData/docker/volumes",
                ".docker",
            ],
        ),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub version: u32,
    pub scanned_at_unix: u64,
    pub roots: Vec<String>,
    pub total_bytes: u64,
    pub total_allocated_bytes: u64,
    pub total_capacity_bytes: Option<u64>,
    pub file_count: u64,
    pub folder_count: u64,
    pub categories: Vec<CategoryTotal>,
    pub largest_files: Vec<SizedEntry>,
    pub largest_folders: Vec<SizedEntry>,
    pub skipped: Vec<SkippedPath>,
    pub access_denied_count: u64,
    #[serde(default)]
    pub category_rules_fingerprint: Option<String>,
    #[serde(default)]
    pub category_rules_source: Option<String>,
    #[serde(default)]
    pub scan_stats: ScanStats,
}

impl Snapshot {
    pub fn stale_info(&self, stale_after_days: u64) -> StaleInfo {
        let age_seconds = current_unix()
            .unwrap_or(self.scanned_at_unix)
            .saturating_sub(self.scanned_at_unix);
        let age_days = age_seconds / 86_400;
        StaleInfo {
            age_days,
            stale_after_days,
            is_stale: stale_after_days > 0 && age_days >= stale_after_days,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ScanStats {
    pub elapsed_ms: u64,
    pub files_per_second: f64,
    pub folders_per_second: f64,
    pub logical_bytes_per_second: f64,
    pub allocated_bytes_per_second: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    BaseScan,
    PresentTracked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheUsageEventKind {
    Summary,
    Files,
    Folders,
    Browse,
    ScanAutoSkip,
    CacheNavigation,
}

impl CacheUsageEventKind {
    fn counts_as_readout(self) -> bool {
        matches!(
            self,
            Self::Summary | Self::Files | Self::Folders | Self::Browse | Self::ScanAutoSkip
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimestampedEvent {
    pub unix_seconds: u64,
    pub utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupJournalEntry {
    #[serde(
        default = "cleanup_journal_version",
        deserialize_with = "deserialize_cleanup_journal_version"
    )]
    pub version: u32,
    pub timestamp: TimestampedEvent,
    pub target_type: String,
    pub path: String,
    pub removed_files: usize,
    pub removed_folders: usize,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub human_size: String,
    pub human_on_disk: String,
    pub entry: SizedEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheUsageJournalEntry {
    #[serde(
        default = "cache_usage_journal_version",
        deserialize_with = "deserialize_cache_usage_journal_version"
    )]
    pub version: u32,
    pub timestamp: TimestampedEvent,
    pub event: CacheUsageEventKind,
    pub counts_as_readout: bool,
    pub basis_scanned_at_unix: u64,
    pub basis_total_bytes: u64,
    pub basis_total_allocated_bytes: u64,
    pub basis_file_count: u64,
    pub basis_folder_count: u64,
    pub basis_elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManualCleanupTotals {
    pub events: usize,
    pub removed_files: usize,
    pub removed_folders: usize,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub human_size: String,
    pub human_on_disk: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MarkRemovedResult {
    pub removed: bool,
    pub target_type: String,
    pub path: String,
    pub removed_files: usize,
    pub removed_folders: usize,
    pub snapshot_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CacheSavingsTotals {
    pub counted_readouts: usize,
    pub estimated_logical_bytes_not_rewalked: u64,
    pub estimated_allocated_bytes_not_rewalked: u64,
    pub estimated_files_not_rechecked: u64,
    pub estimated_folders_not_rechecked: u64,
    pub estimated_scan_time_saved_ms: u64,
    pub estimated_logical_size_not_rewalked: String,
    pub estimated_on_disk_not_rewalked: String,
    pub estimated_scan_time_saved: String,
    pub cache_navigation_count: usize,
    pub basis: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleInfo {
    pub age_days: u64,
    pub stale_after_days: u64,
    pub is_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryRulesStatus {
    pub source: String,
    pub snapshot_fingerprint: Option<String>,
    pub current_fingerprint: String,
    pub changed_since_scan: Option<bool>,
}

pub fn category_rules_status(
    snapshot: &Snapshot,
    category_rules: &CategoryRules,
) -> CategoryRulesStatus {
    CategoryRulesStatus {
        source: category_rules.source().to_string(),
        snapshot_fingerprint: snapshot.category_rules_fingerprint.clone(),
        current_fingerprint: category_rules.fingerprint().to_string(),
        changed_since_scan: snapshot
            .category_rules_fingerprint
            .as_ref()
            .map(|fingerprint| fingerprint != category_rules.fingerprint()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryTotal {
    pub name: String,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizedEntry {
    pub path: String,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub files: Option<u64>,
    pub category: Option<String>,
}

impl Ord for SizedEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| self.allocated_bytes.cmp(&other.allocated_bytes))
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for SizedEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedPath {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditRules {
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub audit_workspace: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub default_thresholds: AuditDefaultThresholds,
    #[serde(default)]
    pub generated_path_names: Vec<String>,
    #[serde(default)]
    pub known_top_level_folders: Vec<String>,
    #[serde(default)]
    pub projects: Vec<AuditProjectRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AuditDefaultThresholds {
    #[serde(default)]
    pub max_size_mb: Option<f64>,
    #[serde(default)]
    pub max_git_size_mb: Option<f64>,
    #[serde(default)]
    pub growth_alert_mb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditProjectRule {
    pub path: String,
    #[serde(default)]
    pub classification: Option<String>,
    #[serde(default)]
    pub max_size_mb: Option<f64>,
    #[serde(default)]
    pub max_git_size_mb: Option<f64>,
    #[serde(default)]
    pub growth_alert_mb: Option<f64>,
    #[serde(default)]
    pub watch_generated_outputs: bool,
    #[serde(default)]
    pub generated_path_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Info,
    Warning,
    Alert,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuditAlert {
    pub severity: AuditSeverity,
    pub rule_id: String,
    pub path: String,
    pub message: String,
    pub measured_bytes: Option<u64>,
    pub threshold_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuditFolderDelta {
    pub path: String,
    pub previous_bytes: u64,
    pub current_bytes: u64,
    pub delta_bytes: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuditReport {
    pub schema_version: u32,
    pub command: String,
    pub workspace: String,
    pub roots: Vec<String>,
    pub generated_at_unix: u64,
    pub generated_at_utc: String,
    pub scanned_at_unix: u64,
    pub scanned_at_utc: String,
    pub reused_cached_scan: bool,
    pub alerts: Vec<AuditAlert>,
    pub largest_folders: Vec<SizedEntry>,
    pub growth: Vec<AuditFolderDelta>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuditRulesValidation {
    pub valid: bool,
    pub rule_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationTarget {
    File,
    Folder,
}

fn navigation_target_label(target: NavigationTarget) -> &'static str {
    match target {
        NavigationTarget::File => "file",
        NavigationTarget::Folder => "folder",
    }
}

fn navigation_target_from_label(label: &str) -> NavigationTarget {
    match label {
        "file" => NavigationTarget::File,
        "folder" => NavigationTarget::Folder,
        _ => NavigationTarget::Folder,
    }
}

#[derive(Debug, Default)]
struct CategoryAccumulator {
    bytes: u64,
    allocated_bytes: u64,
    files: u64,
}

#[derive(Debug, Default)]
struct PartialScan {
    total_bytes: u64,
    total_allocated_bytes: u64,
    file_count: u64,
    folder_count: u64,
    categories: HashMap<String, CategoryAccumulator>,
    largest_files: TopList,
    folder_bytes: HashMap<PathBuf, (u64, u64)>,
    folder_files: HashMap<PathBuf, u64>,
    skipped: Vec<SkippedPath>,
    access_denied_count: u64,
}

impl PartialScan {
    fn with_limit(limit: usize) -> Self {
        Self {
            largest_files: TopList::new(limit),
            ..Self::default()
        }
    }

    fn merge(&mut self, other: PartialScan) {
        self.total_bytes = self.total_bytes.saturating_add(other.total_bytes);
        self.total_allocated_bytes = self
            .total_allocated_bytes
            .saturating_add(other.total_allocated_bytes);
        self.file_count = self.file_count.saturating_add(other.file_count);
        self.folder_count = self.folder_count.saturating_add(other.folder_count);
        self.access_denied_count = self
            .access_denied_count
            .saturating_add(other.access_denied_count);
        self.skipped.extend(other.skipped);
        self.largest_files
            .extend(other.largest_files.into_sorted_vec());

        for (name, total) in other.categories {
            let entry = self.categories.entry(name).or_default();
            entry.bytes = entry.bytes.saturating_add(total.bytes);
            entry.allocated_bytes = entry.allocated_bytes.saturating_add(total.allocated_bytes);
            entry.files = entry.files.saturating_add(total.files);
        }

        for (path, (bytes, allocated_bytes)) in other.folder_bytes {
            let entry = self.folder_bytes.entry(path).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(bytes);
            entry.1 = entry.1.saturating_add(allocated_bytes);
        }

        for (path, files) in other.folder_files {
            let entry = self.folder_files.entry(path).or_insert(0);
            *entry = entry.saturating_add(files);
        }
    }
}

#[derive(Debug, Default)]
struct TopList {
    limit: usize,
    heap: BinaryHeap<std::cmp::Reverse<SizedEntry>>,
}

impl TopList {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
        }
    }

    fn push(&mut self, entry: SizedEntry) {
        if self.limit == 0 {
            return;
        }

        self.heap.push(std::cmp::Reverse(entry));
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    fn extend(&mut self, entries: Vec<SizedEntry>) {
        for entry in entries {
            self.push(entry);
        }
    }

    fn into_sorted_vec(self) -> Vec<SizedEntry> {
        let mut entries = self
            .heap
            .into_iter()
            .map(|std::cmp::Reverse(entry)| entry)
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        entries
    }
}

pub fn save_snapshot(paths: &CachePaths, snapshot: &Snapshot) -> Result<(), CliError> {
    save_snapshot_to(&paths.snapshot_path, paths, snapshot)
}

fn save_base_snapshot(paths: &CachePaths, snapshot: &Snapshot) -> Result<(), CliError> {
    save_snapshot_to(&paths.base_snapshot_path, paths, snapshot)
}

fn save_snapshot_to(path: &Path, paths: &CachePaths, snapshot: &Snapshot) -> Result<(), CliError> {
    fs::create_dir_all(&paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create cache directory {}",
            paths.base_dir.display()
        ),
        source,
    })?;
    let json = serde_json::to_string_pretty(snapshot).map_err(|source| CliError::Json {
        context: "failed to serialize snapshot".to_string(),
        source,
    })?;
    fs::write(path, json).map_err(|source| CliError::Io {
        context: format!("failed to write snapshot {}", path.display()),
        source,
    })
}

pub fn load_snapshot(paths: &CachePaths) -> Result<Snapshot, CliError> {
    let contents = fs::read_to_string(&paths.snapshot_path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CliError::Message(format!(
                "no cached scan found at {}; run `dscan11 scan` first",
                paths.snapshot_path.display()
            ))
        } else {
            CliError::Io {
                context: format!("failed to read snapshot {}", paths.snapshot_path.display()),
                source,
            }
        }
    })?;
    let snapshot: Snapshot = serde_json::from_str(&contents).map_err(|source| CliError::Json {
        context: format!("failed to parse snapshot {}", paths.snapshot_path.display()),
        source,
    })?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(CliError::Message(format!(
            "snapshot version {} is unsupported by this dscan11 version {}; run `dscan11 scan` to refresh",
            snapshot.version, SNAPSHOT_VERSION
        )));
    }
    Ok(snapshot)
}

pub fn load_base_snapshot(paths: &CachePaths) -> Result<Snapshot, CliError> {
    load_snapshot_from(&paths.base_snapshot_path)
}

fn load_snapshot_from(path: &Path) -> Result<Snapshot, CliError> {
    let contents = fs::read_to_string(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CliError::Message(format!(
                "no base scan found at {}; run `dscan11 scan` first",
                path.display()
            ))
        } else {
            CliError::Io {
                context: format!("failed to read snapshot {}", path.display()),
                source,
            }
        }
    })?;
    let snapshot: Snapshot = serde_json::from_str(&contents).map_err(|source| CliError::Json {
        context: format!("failed to parse snapshot {}", path.display()),
        source,
    })?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(CliError::Message(format!(
            "snapshot version {} is unsupported by this dscan11 version {}; run `dscan11 scan` to refresh",
            snapshot.version, SNAPSHOT_VERSION
        )));
    }
    Ok(snapshot)
}

pub fn save_full_scan(paths: &CachePaths, snapshot: &Snapshot) -> Result<(), CliError> {
    archive_tracking_files(paths)?;
    save_base_snapshot(paths, snapshot)?;
    save_snapshot(paths, snapshot)?;
    reset_active_journals(paths)
}

pub fn audit_scan_top_limit(config: &AppConfig, display_limit: usize) -> usize {
    config
        .top_limit
        .max(DEFAULT_AUDIT_TOP_LIMIT)
        .max(display_limit.max(1))
}

pub fn load_audit_rules(path: &Path) -> Result<AuditRules, CliError> {
    let contents = fs::read_to_string(path).map_err(|source| CliError::Io {
        context: format!("failed to read audit rules {}", path.display()),
        source,
    })?;
    serde_json::from_str(&contents).map_err(|source| CliError::Json {
        context: format!("failed to parse audit rules {}", path.display()),
        source,
    })
}

pub fn validate_audit_rules(rules: &AuditRules) -> Result<AuditRulesValidation, CliError> {
    if let Some(version) = rules.schema_version {
        if version > AUDIT_REPORT_VERSION {
            return Err(CliError::Message(format!(
                "unsupported audit rules version {version}; this dscan11 supports {AUDIT_REPORT_VERSION}"
            )));
        }
    }

    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    for project in &rules.projects {
        if project.path.trim().is_empty() {
            return Err(CliError::Message(
                "audit project path must not be empty".to_string(),
            ));
        }
        let normalized = project.path.replace('\\', "/").to_ascii_lowercase();
        if normalized.contains("..") || normalized.starts_with('/') {
            return Err(CliError::Message(format!(
                "audit project path `{}` must be relative and must not contain `..`",
                project.path
            )));
        }
        if !seen.insert(normalized) {
            warnings.push(format!("duplicate project rule for `{}`", project.path));
        }
        for (label, value) in [
            ("max_size_mb", project.max_size_mb),
            ("max_git_size_mb", project.max_git_size_mb),
            ("growth_alert_mb", project.growth_alert_mb),
        ] {
            validate_optional_mb(label, value)?;
        }
    }
    for (label, value) in [
        ("default max_size_mb", rules.default_thresholds.max_size_mb),
        (
            "default max_git_size_mb",
            rules.default_thresholds.max_git_size_mb,
        ),
        (
            "default growth_alert_mb",
            rules.default_thresholds.growth_alert_mb,
        ),
    ] {
        validate_optional_mb(label, value)?;
    }
    Ok(AuditRulesValidation {
        valid: true,
        rule_count: rules.projects.len(),
        warnings,
    })
}

fn validate_optional_mb(label: &str, value: Option<f64>) -> Result<(), CliError> {
    if let Some(value) = value {
        if !value.is_finite() || value < 0.0 {
            return Err(CliError::Message(format!(
                "audit threshold `{label}` must be a non-negative finite number"
            )));
        }
    }
    Ok(())
}

pub fn default_audit_rules() -> AuditRules {
    AuditRules {
        schema_version: Some(AUDIT_REPORT_VERSION),
        audit_workspace: None,
        root: None,
        default_thresholds: AuditDefaultThresholds {
            max_size_mb: Some(500.0),
            max_git_size_mb: Some(100.0),
            growth_alert_mb: Some(250.0),
        },
        generated_path_names: vec![
            "target".to_string(),
            "target-fresh".to_string(),
            ".venv".to_string(),
            "node_modules".to_string(),
            "dist".to_string(),
            ".cache".to_string(),
        ],
        known_top_level_folders: Vec::new(),
        projects: Vec::new(),
    }
}

pub fn write_default_audit_rules(path: &Path) -> Result<(), CliError> {
    if path.exists() {
        return Err(CliError::Message(format!(
            "audit rules already exist: {}",
            path.display()
        )));
    }
    let rules = default_audit_rules();
    let json = serde_json::to_string_pretty(&rules).map_err(|source| CliError::Json {
        context: "failed to serialize audit rules".to_string(),
        source,
    })?;
    fs::write(path, json).map_err(|source| CliError::Io {
        context: format!("failed to write audit rules {}", path.display()),
        source,
    })
}

pub fn audit_rules_from_inventory(path: &Path) -> Result<AuditRules, CliError> {
    let contents = fs::read_to_string(path).map_err(|source| CliError::Io {
        context: format!("failed to read workspace inventory {}", path.display()),
        source,
    })?;
    let mut rules = default_audit_rules();
    let mut current_folder: Option<String> = None;
    let mut current_classification: Option<String> = None;
    for line in contents.lines() {
        if let Some(folder) = parse_inventory_heading(line) {
            if let Some(previous) = current_folder.replace(folder) {
                rules.projects.push(inventory_project_rule(
                    previous,
                    current_classification.take(),
                ));
            }
        } else if let Some(classification) = parse_inventory_classification(line) {
            current_classification = Some(classification);
        }
    }
    if let Some(previous) = current_folder {
        rules.projects.push(inventory_project_rule(
            previous,
            current_classification.take(),
        ));
    }
    rules.known_top_level_folders = rules
        .projects
        .iter()
        .filter_map(|project| project.path.split(['\\', '/']).next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    rules.known_top_level_folders.sort();
    validate_audit_rules(&rules)?;
    Ok(rules)
}

fn inventory_project_rule(path: String, classification: Option<String>) -> AuditProjectRule {
    let lower = path.to_ascii_lowercase();
    let (max_size_mb, max_git_size_mb) = if lower.contains("dscan11") {
        (Some(250.0), Some(25.0))
    } else {
        (Some(500.0), Some(100.0))
    };
    AuditProjectRule {
        path,
        classification,
        max_size_mb,
        max_git_size_mb,
        growth_alert_mb: None,
        watch_generated_outputs: true,
        generated_path_names: None,
    }
}

fn parse_inventory_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("### `") {
        return None;
    }
    let rest = trimmed.trim_start_matches("### `");
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn parse_inventory_classification(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("- `Classification`:") {
        return None;
    }
    Some(
        trimmed
            .trim_start_matches("- `Classification`:")
            .trim()
            .to_string(),
    )
}

pub fn build_audit_report(
    paths: &CachePaths,
    snapshot: &Snapshot,
    previous_snapshot: Option<&Snapshot>,
    rules: &AuditRules,
    limit: usize,
    reused_cached_scan: bool,
) -> Result<AuditReport, CliError> {
    validate_audit_rules(rules)?;
    let generated_at_unix = current_unix()?;
    let mut alerts = Vec::new();
    let root = audit_root(snapshot)?;
    let folder_index = audit_folder_index(snapshot);
    let previous_index = previous_snapshot.map(audit_folder_index);
    let known_top_level = rules
        .known_top_level_folders
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<_>>();

    if !known_top_level.is_empty() && root.exists() {
        let entries = fs::read_dir(&root).map_err(|source| CliError::Io {
            context: format!("failed to read audit root {}", root.display()),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| CliError::Io {
                context: format!("failed to read entry under {}", root.display()),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| CliError::Io {
                context: format!("failed to read type for {}", entry.path().display()),
                source,
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            if !known_top_level.contains(&name.to_ascii_lowercase()) {
                alerts.push(AuditAlert {
                    severity: AuditSeverity::Warning,
                    rule_id: "unknown_top_level_folder".to_string(),
                    path: entry.path().display().to_string(),
                    message: "Unregistered top-level folder is present under the audit root."
                        .to_string(),
                    measured_bytes: None,
                    threshold_bytes: None,
                });
            }
        }
    }

    let global_generated = normalized_name_set(&rules.generated_path_names);
    for project in &rules.projects {
        let project_path = root.join(&project.path);
        let project_key = normalize_path_string(&project_path);
        let project_size = folder_index.get(&project_key).map(|entry| entry.bytes);
        let max_size = project
            .max_size_mb
            .or(rules.default_thresholds.max_size_mb)
            .and_then(mb_to_bytes);
        if let (Some(size), Some(threshold)) = (project_size, max_size) {
            if size > threshold {
                alerts.push(AuditAlert {
                    severity: AuditSeverity::Alert,
                    rule_id: "project_size".to_string(),
                    path: project_path.display().to_string(),
                    message: format!(
                        "Project is {}; threshold is {}.",
                        human_size(size),
                        human_size(threshold)
                    ),
                    measured_bytes: Some(size),
                    threshold_bytes: Some(threshold),
                });
            }
        }

        let git_path = project_path.join(".git");
        let git_key = normalize_path_string(&git_path);
        let git_size = folder_index.get(&git_key).map(|entry| entry.bytes);
        let max_git = project
            .max_git_size_mb
            .or(rules.default_thresholds.max_git_size_mb)
            .and_then(mb_to_bytes);
        if let (Some(size), Some(threshold)) = (git_size, max_git) {
            if size > threshold {
                alerts.push(AuditAlert {
                    severity: AuditSeverity::Alert,
                    rule_id: "git_size".to_string(),
                    path: git_path.display().to_string(),
                    message: format!(
                        ".git is {}; threshold is {}.",
                        human_size(size),
                        human_size(threshold)
                    ),
                    measured_bytes: Some(size),
                    threshold_bytes: Some(threshold),
                });
            }
        }

        if project.watch_generated_outputs {
            let generated_names = project
                .generated_path_names
                .as_ref()
                .map(|names| normalized_name_set(names))
                .unwrap_or_else(|| global_generated.clone());
            if !generated_names.is_empty() {
                let project_prefix = normalize_path_prefix(&project_path);
                for entry in &snapshot.largest_folders {
                    let entry_path = PathBuf::from(&entry.path);
                    let normalized = normalize_path_string(&entry_path);
                    if !normalized.starts_with(&project_prefix) {
                        continue;
                    }
                    let Some(name) = entry_path.file_name().and_then(OsStr::to_str) else {
                        continue;
                    };
                    if generated_names.contains(&name.to_ascii_lowercase()) {
                        alerts.push(AuditAlert {
                            severity: AuditSeverity::Warning,
                            rule_id: "generated_output".to_string(),
                            path: entry.path.clone(),
                            message: "Generated output folder detected.".to_string(),
                            measured_bytes: Some(entry.bytes),
                            threshold_bytes: None,
                        });
                    }
                }
            }
        }
    }

    let growth = rules
        .projects
        .iter()
        .filter_map(|project| {
            let previous_index = previous_index.as_ref()?;
            let project_path = root.join(&project.path);
            let key = normalize_path_string(&project_path);
            let current = folder_index.get(&key)?.bytes;
            let previous = previous_index.get(&key)?.bytes;
            let delta = current as i128 - previous as i128;
            let clamped = delta.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            let threshold = project
                .growth_alert_mb
                .or(rules.default_thresholds.growth_alert_mb)
                .and_then(mb_to_bytes);
            if let Some(threshold) = threshold {
                if delta > i128::from(threshold) {
                    alerts.push(AuditAlert {
                        severity: AuditSeverity::Warning,
                        rule_id: "project_growth".to_string(),
                        path: project_path.display().to_string(),
                        message: format!(
                            "Project grew by {}; threshold is {}.",
                            human_size(delta as u64),
                            human_size(threshold)
                        ),
                        measured_bytes: Some(delta as u64),
                        threshold_bytes: Some(threshold),
                    });
                }
            }
            Some(AuditFolderDelta {
                path: project_path.display().to_string(),
                previous_bytes: previous,
                current_bytes: current,
                delta_bytes: clamped,
            })
        })
        .collect::<Vec<_>>();

    Ok(AuditReport {
        schema_version: AUDIT_REPORT_VERSION,
        command: "audit".to_string(),
        workspace: paths.workspace_name.clone(),
        roots: snapshot.roots.clone(),
        generated_at_unix,
        generated_at_utc: format_unix_utc(generated_at_unix)?,
        scanned_at_unix: snapshot.scanned_at_unix,
        scanned_at_utc: format_unix_utc(snapshot.scanned_at_unix)?,
        reused_cached_scan,
        alerts,
        largest_folders: snapshot
            .largest_folders
            .iter()
            .take(limit)
            .cloned()
            .collect(),
        growth,
    })
}

pub fn print_audit_report(report: &AuditReport, output: OutputMode) -> Result<(), CliError> {
    if output.is_json() {
        return print_json(report);
    }

    println!("Audit report");
    println!("  workspace: {}", report.workspace);
    println!("  scanned at: {}", report.scanned_at_utc);
    println!("  generated at: {}", report.generated_at_utc);
    println!("  roots: {}", report.roots.join(", "));
    println!("  alerts: {}", report.alerts.len());
    if report.alerts.is_empty() {
        println!("  no audit thresholds were exceeded");
    } else {
        println!();
        println!("Alerts");
        for alert in &report.alerts {
            println!("  [{:?}] {}: {}", alert.severity, alert.path, alert.message);
        }
    }
    if !report.largest_folders.is_empty() {
        println!();
        println!("Largest folders");
        for (index, folder) in report.largest_folders.iter().enumerate() {
            println!(
                "  {}. {}  {}",
                index + 1,
                human_size(folder.bytes),
                folder.path
            );
        }
    }
    Ok(())
}

fn audit_root(snapshot: &Snapshot) -> Result<PathBuf, CliError> {
    if snapshot.roots.len() != 1 {
        return Err(CliError::Message(
            "audit requires a snapshot with exactly one root".to_string(),
        ));
    }
    Ok(PathBuf::from(&snapshot.roots[0]))
}

fn audit_folder_index(snapshot: &Snapshot) -> HashMap<String, SizedEntry> {
    snapshot
        .largest_folders
        .iter()
        .cloned()
        .map(|entry| (normalize_path_string(Path::new(&entry.path)), entry))
        .collect()
}

fn normalized_name_set(names: &[String]) -> HashSet<String> {
    names.iter().map(|name| name.to_ascii_lowercase()).collect()
}

fn normalize_path_prefix(path: &Path) -> String {
    let mut value = normalize_path_string(path);
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn normalize_path_string(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn mb_to_bytes(mb: f64) -> Option<u64> {
    if !mb.is_finite() || mb < 0.0 {
        return None;
    }
    Some((mb * 1_048_576.0).round() as u64)
}

fn archive_tracking_files(paths: &CachePaths) -> Result<(), CliError> {
    let timestamp = current_unix()?;
    fs::create_dir_all(&paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create cache directory {}",
            paths.base_dir.display()
        ),
        source,
    })?;
    for path in [
        &paths.base_snapshot_path,
        &paths.cleanup_journal_path,
        &paths.cache_usage_journal_path,
    ] {
        if path.exists() {
            let file_name = path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("cache-file");
            let archive_path = paths
                .base_dir
                .join(format!("{file_name}.{timestamp}.archive"));
            fs::rename(path, &archive_path).map_err(|source| CliError::Io {
                context: format!(
                    "failed to archive {} to {}",
                    path.display(),
                    archive_path.display()
                ),
                source,
            })?;
        }
    }
    Ok(())
}

fn reset_active_journals(paths: &CachePaths) -> Result<(), CliError> {
    fs::write(&paths.cleanup_journal_path, "").map_err(|source| CliError::Io {
        context: format!(
            "failed to reset cleanup journal {}",
            paths.cleanup_journal_path.display()
        ),
        source,
    })?;
    fs::write(&paths.cache_usage_journal_path, "").map_err(|source| CliError::Io {
        context: format!(
            "failed to reset cache usage journal {}",
            paths.cache_usage_journal_path.display()
        ),
        source,
    })
}

pub fn restore_base_cache(paths: &CachePaths) -> Result<(), CliError> {
    let base = load_base_snapshot(paths)?;
    save_snapshot(paths, &base)
}

pub fn fast_forward_cache(paths: &CachePaths) -> Result<(), CliError> {
    let mut snapshot = load_base_snapshot(paths)?;
    for entry in load_cleanup_journal(paths)? {
        prune_cached_entry(
            &mut snapshot,
            navigation_target_from_label(&entry.target_type),
            &entry.entry,
        );
    }
    save_snapshot(paths, &snapshot)
}

pub fn mark_cached_path_removed(
    paths: &CachePaths,
    target: NavigationTarget,
    path: &Path,
) -> Result<MarkRemovedResult, CliError> {
    let mut snapshot = load_snapshot(paths)?;
    let requested_path = path.display().to_string();
    let entry = find_cached_entry_by_path(&snapshot, target, &requested_path)
        .cloned()
        .ok_or_else(|| {
            CliError::Message(format!(
                "cached {} path not found in active snapshot: {}",
                navigation_target_label(target),
                requested_path
            ))
        })?;
    if path_exists(path)? {
        return Err(CliError::Message(format!(
            "cached {} still exists on disk: {}",
            navigation_target_label(target),
            requested_path
        )));
    }

    let result = prune_cached_entry(&mut snapshot, target, &entry);
    if result.removed_files == 0 && result.removed_folders == 0 {
        return Err(CliError::Message(format!(
            "cached {} path not found in active snapshot: {}",
            navigation_target_label(target),
            requested_path
        )));
    }

    append_cleanup_journal(paths, target, &entry, &result)?;
    save_snapshot(paths, &snapshot)?;
    Ok(MarkRemovedResult {
        removed: true,
        target_type: navigation_target_label(target).to_string(),
        path: entry.path,
        removed_files: result.removed_files,
        removed_folders: result.removed_folders,
        snapshot_path: paths.snapshot_path.display().to_string(),
    })
}

fn path_exists(path: &Path) -> Result<bool, CliError> {
    path.try_exists().map_err(|source| CliError::Io {
        context: format!("failed to check whether path exists {}", path.display()),
        source,
    })
}

fn find_cached_entry_by_path<'a>(
    snapshot: &'a Snapshot,
    target: NavigationTarget,
    path: &str,
) -> Option<&'a SizedEntry> {
    let entries = match target {
        NavigationTarget::File => &snapshot.largest_files,
        NavigationTarget::Folder => &snapshot.largest_folders,
    };
    entries.iter().find(|entry| same_path(&entry.path, path))
}

pub fn print_cleanup_journal(paths: &CachePaths, output: OutputMode) -> Result<(), CliError> {
    let entries = load_cleanup_journal(paths)?;
    if output.is_json() {
        return print_json(&entries);
    }

    if entries.is_empty() {
        println!("No manual cleanup entries.");
        return Ok(());
    }

    println!("Manual cleanup journal");
    for (index, entry) in entries.iter().enumerate() {
        println!(
            "  {}. {} {} ({}, {} file(s), {} folder(s))",
            index + 1,
            entry.timestamp.utc,
            entry.path,
            entry.human_on_disk,
            entry.removed_files,
            entry.removed_folders
        );
    }
    Ok(())
}

pub fn cache_mode(paths: &CachePaths, snapshot: &Snapshot) -> CacheMode {
    match load_base_snapshot(paths) {
        Ok(base) if &base == snapshot => CacheMode::BaseScan,
        _ => CacheMode::PresentTracked,
    }
}

pub fn record_cache_usage(paths: &CachePaths, event: CacheUsageEventKind) -> Result<(), CliError> {
    let basis = load_base_snapshot(paths).or_else(|_| load_snapshot(paths))?;
    let entry = CacheUsageJournalEntry {
        version: CACHE_USAGE_JOURNAL_VERSION,
        timestamp: timestamped_event()?,
        event,
        counts_as_readout: event.counts_as_readout(),
        basis_scanned_at_unix: basis.scanned_at_unix,
        basis_total_bytes: basis.total_bytes,
        basis_total_allocated_bytes: basis.total_allocated_bytes,
        basis_file_count: basis.file_count,
        basis_folder_count: basis.folder_count,
        basis_elapsed_ms: basis.scan_stats.elapsed_ms,
    };
    append_jsonl(&paths.cache_usage_journal_path, paths, &entry)
}

fn append_cleanup_journal(
    paths: &CachePaths,
    target: NavigationTarget,
    entry: &SizedEntry,
    result: &PruneResult,
) -> Result<(), CliError> {
    let journal_entry = CleanupJournalEntry {
        version: CLEANUP_JOURNAL_VERSION,
        timestamp: timestamped_event()?,
        target_type: navigation_target_label(target).to_string(),
        path: entry.path.clone(),
        removed_files: result.removed_files,
        removed_folders: result.removed_folders,
        bytes: entry.bytes,
        allocated_bytes: entry.allocated_bytes,
        human_size: human_size(entry.bytes),
        human_on_disk: human_size(entry.allocated_bytes),
        entry: entry.clone(),
    };
    append_jsonl(&paths.cleanup_journal_path, paths, &journal_entry)
}

fn append_jsonl<T: Serialize>(path: &Path, paths: &CachePaths, value: &T) -> Result<(), CliError> {
    fs::create_dir_all(&paths.base_dir).map_err(|source| CliError::Io {
        context: format!(
            "failed to create cache directory {}",
            paths.base_dir.display()
        ),
        source,
    })?;
    let json = serde_json::to_string(value).map_err(|source| CliError::Json {
        context: "failed to serialize journal entry".to_string(),
        source,
    })?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| CliError::Io {
            context: format!("failed to open journal {}", path.display()),
            source,
        })?;
    writeln!(file, "{json}").map_err(|source| CliError::Io {
        context: format!("failed to write journal {}", path.display()),
        source,
    })
}

fn load_cleanup_journal(paths: &CachePaths) -> Result<Vec<CleanupJournalEntry>, CliError> {
    load_jsonl(&paths.cleanup_journal_path, "cleanup journal")
}

fn load_cache_usage_journal(paths: &CachePaths) -> Result<Vec<CacheUsageJournalEntry>, CliError> {
    load_jsonl(&paths.cache_usage_journal_path, "cache usage journal")
}

fn load_jsonl<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<Vec<T>, CliError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(CliError::Io {
                context: format!("failed to read {label} {}", path.display()),
                source,
            });
        }
    };
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| CliError::Json {
                context: format!(
                    "failed to parse {label} {} line {}",
                    path.display(),
                    index + 1
                ),
                source,
            })
        })
        .collect()
}

pub fn manual_cleanup_totals(paths: &CachePaths) -> Result<ManualCleanupTotals, CliError> {
    let entries = load_cleanup_journal(paths)?;
    let removed_files = entries
        .iter()
        .map(|entry| entry.removed_files)
        .sum::<usize>();
    let removed_folders = entries
        .iter()
        .map(|entry| entry.removed_folders)
        .sum::<usize>();
    let bytes = entries
        .iter()
        .map(|entry| entry.bytes)
        .fold(0u64, u64::saturating_add);
    let allocated_bytes = entries
        .iter()
        .map(|entry| entry.allocated_bytes)
        .fold(0u64, u64::saturating_add);
    Ok(ManualCleanupTotals {
        events: entries.len(),
        removed_files,
        removed_folders,
        bytes,
        allocated_bytes,
        human_size: human_size(bytes),
        human_on_disk: human_size(allocated_bytes),
    })
}

pub fn cache_savings_totals(paths: &CachePaths) -> Result<CacheSavingsTotals, CliError> {
    let entries = load_cache_usage_journal(paths)?;
    let counted = entries
        .iter()
        .filter(|entry| entry.counts_as_readout)
        .collect::<Vec<_>>();
    let estimated_logical_bytes_not_rewalked = counted
        .iter()
        .map(|entry| entry.basis_total_bytes)
        .fold(0u64, u64::saturating_add);
    let estimated_allocated_bytes_not_rewalked = counted
        .iter()
        .map(|entry| entry.basis_total_allocated_bytes)
        .fold(0u64, u64::saturating_add);
    let estimated_files_not_rechecked = counted
        .iter()
        .map(|entry| entry.basis_file_count)
        .fold(0u64, u64::saturating_add);
    let estimated_folders_not_rechecked = counted
        .iter()
        .map(|entry| entry.basis_folder_count)
        .fold(0u64, u64::saturating_add);
    let estimated_scan_time_saved_ms = counted
        .iter()
        .map(|entry| entry.basis_elapsed_ms)
        .fold(0u64, u64::saturating_add);
    let cache_navigation_count = entries
        .iter()
        .filter(|entry| entry.event == CacheUsageEventKind::CacheNavigation)
        .count();
    Ok(CacheSavingsTotals {
        counted_readouts: counted.len(),
        estimated_logical_bytes_not_rewalked,
        estimated_allocated_bytes_not_rewalked,
        estimated_files_not_rechecked,
        estimated_folders_not_rechecked,
        estimated_scan_time_saved_ms,
        estimated_logical_size_not_rewalked: human_size(estimated_logical_bytes_not_rewalked),
        estimated_on_disk_not_rewalked: human_size(estimated_allocated_bytes_not_rewalked),
        estimated_scan_time_saved: human_duration(estimated_scan_time_saved_ms),
        cache_navigation_count,
        basis: "Each counted cache readout estimates one avoided full readout of the last scanned scope.".to_string(),
    })
}

fn timestamped_event() -> Result<TimestampedEvent, CliError> {
    let now = OffsetDateTime::now_utc();
    let unix_seconds = now.unix_timestamp().try_into().map_err(|_| {
        CliError::Message("system clock is before Unix epoch; cannot write journal".to_string())
    })?;
    let utc = now.format(&Rfc3339).map_err(|source| {
        CliError::Message(format!("failed to format journal timestamp: {source}"))
    })?;
    Ok(TimestampedEvent { unix_seconds, utc })
}

fn format_unix_utc(unix_seconds: u64) -> Result<String, CliError> {
    let unix_seconds = i64::try_from(unix_seconds)
        .map_err(|_| CliError::Message("scan timestamp is outside supported range".to_string()))?;
    OffsetDateTime::from_unix_timestamp(unix_seconds)
        .map_err(|source| CliError::Message(format!("scan timestamp is invalid: {source}")))?
        .format(&Rfc3339)
        .map_err(|source| CliError::Message(format!("failed to format scan timestamp: {source}")))
}

pub fn scan_paths(
    roots: &[PathBuf],
    config: &AppConfig,
    category_rules: &CategoryRules,
    top_limit: usize,
) -> Result<Snapshot, CliError> {
    let started = Instant::now();
    if roots.is_empty() {
        return Err(CliError::Message("no scan roots were found".to_string()));
    }
    for root in roots {
        if !root.is_dir() {
            return Err(CliError::Message(format!(
                "scan root does not exist or is not a directory: {}",
                root.display()
            )));
        }
    }

    let partials = roots
        .par_iter()
        .map(|root| scan_root(root, config, category_rules, top_limit))
        .collect::<Vec<_>>();

    let mut merged = PartialScan::with_limit(top_limit);
    for partial in partials {
        merged.merge(partial);
    }

    let mut categories = merged
        .categories
        .into_iter()
        .map(|(name, total)| CategoryTotal {
            name,
            bytes: total.bytes,
            allocated_bytes: total.allocated_bytes,
            files: total.files,
        })
        .collect::<Vec<_>>();
    categories.sort_by(|a, b| {
        b.allocated_bytes
            .cmp(&a.allocated_bytes)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut largest_folders = merged
        .folder_bytes
        .into_iter()
        .map(|(path, (bytes, allocated_bytes))| SizedEntry {
            files: merged.folder_files.get(&path).copied(),
            path: path.display().to_string(),
            bytes,
            allocated_bytes,
            category: None,
        })
        .collect::<Vec<_>>();
    largest_folders.sort_by(|a, b| {
        b.allocated_bytes
            .cmp(&a.allocated_bytes)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
    largest_folders.truncate(top_limit);

    let elapsed = started.elapsed();
    let elapsed_seconds = elapsed.as_secs_f64();
    let scan_stats = ScanStats {
        elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
        files_per_second: rate(merged.file_count, elapsed_seconds),
        folders_per_second: rate(merged.folder_count, elapsed_seconds),
        logical_bytes_per_second: rate(merged.total_bytes, elapsed_seconds),
        allocated_bytes_per_second: rate(merged.total_allocated_bytes, elapsed_seconds),
    };

    Ok(Snapshot {
        version: SNAPSHOT_VERSION,
        scanned_at_unix: current_unix()?,
        roots: roots
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        total_bytes: merged.total_bytes,
        total_allocated_bytes: merged.total_allocated_bytes,
        total_capacity_bytes: total_capacity_for_roots(roots),
        file_count: merged.file_count,
        folder_count: merged.folder_count,
        categories,
        largest_files: merged.largest_files.into_sorted_vec(),
        largest_folders,
        skipped: merged.skipped,
        access_denied_count: merged.access_denied_count,
        category_rules_fingerprint: Some(category_rules.fingerprint().to_string()),
        category_rules_source: Some(category_rules.source().to_string()),
        scan_stats,
    })
}

fn rate(value: u64, elapsed_seconds: f64) -> f64 {
    if elapsed_seconds > 0.0 {
        value as f64 / elapsed_seconds
    } else {
        0.0
    }
}

fn scan_root(
    root: &Path,
    config: &AppConfig,
    category_rules: &CategoryRules,
    top_limit: usize,
) -> PartialScan {
    let mut scan = PartialScan::with_limit(top_limit);
    let root = root.to_path_buf();
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        if should_skip_dir(&dir, &root, &config.skip_names) {
            scan.skipped.push(SkippedPath {
                path: dir.display().to_string(),
                reason: "configured protected directory".to_string(),
            });
            continue;
        }

        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                scan.skipped.push(SkippedPath {
                    path: dir.display().to_string(),
                    reason: error_reason(&err),
                });
                if err.kind() == io::ErrorKind::PermissionDenied {
                    scan.access_denied_count = scan.access_denied_count.saturating_add(1);
                }
                continue;
            }
        };

        scan.folder_count = scan.folder_count.saturating_add(1);

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    scan.skipped.push(SkippedPath {
                        path: dir.display().to_string(),
                        reason: error_reason(&err),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    scan.skipped.push(SkippedPath {
                        path: path.display().to_string(),
                        reason: error_reason(&err),
                    });
                    continue;
                }
            };

            if file_type.is_symlink() {
                scan.skipped.push(SkippedPath {
                    path: path.display().to_string(),
                    reason: "symbolic link".to_string(),
                });
                continue;
            }

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    scan.skipped.push(SkippedPath {
                        path: path.display().to_string(),
                        reason: error_reason(&err),
                    });
                    continue;
                }
            };
            let bytes = metadata.len();
            let allocated_bytes = allocated_size(&path).unwrap_or(bytes);
            let category = category_rules.classify(&path);

            scan.total_bytes = scan.total_bytes.saturating_add(bytes);
            scan.total_allocated_bytes = scan.total_allocated_bytes.saturating_add(allocated_bytes);
            scan.file_count = scan.file_count.saturating_add(1);
            let category_total = scan.categories.entry(category.clone()).or_default();
            category_total.bytes = category_total.bytes.saturating_add(bytes);
            category_total.allocated_bytes = category_total
                .allocated_bytes
                .saturating_add(allocated_bytes);
            category_total.files = category_total.files.saturating_add(1);

            scan.largest_files.push(SizedEntry {
                path: path.display().to_string(),
                bytes,
                allocated_bytes,
                files: None,
                category: Some(category),
            });
            add_to_folder_rollups(&mut scan, &root, &path, bytes, allocated_bytes);
        }
    }

    scan
}

fn add_to_folder_rollups(
    scan: &mut PartialScan,
    root: &Path,
    file_path: &Path,
    bytes: u64,
    allocated_bytes: u64,
) {
    let mut current = file_path.parent();

    while let Some(dir) = current {
        if !dir.starts_with(root) {
            break;
        }

        let bytes_entry = scan.folder_bytes.entry(dir.to_path_buf()).or_insert((0, 0));
        bytes_entry.0 = bytes_entry.0.saturating_add(bytes);
        bytes_entry.1 = bytes_entry.1.saturating_add(allocated_bytes);
        let files_entry = scan.folder_files.entry(dir.to_path_buf()).or_insert(0);
        *files_entry = files_entry.saturating_add(1);

        if dir == root {
            break;
        }
        current = dir.parent();
    }
}

fn should_skip_dir(path: &Path, root: &Path, skip_names: &[String]) -> bool {
    if path == root {
        return false;
    }

    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    skip_names
        .iter()
        .any(|skip| skip.eq_ignore_ascii_case(name))
}

fn error_reason(err: &io::Error) -> String {
    format!("{:?}", err.kind())
}

pub fn classify_path(path: &Path) -> String {
    CategoryRules::from_config(CategoryConfig::default(), "builtin".to_string())
        .expect("default category config is valid")
        .classify(path)
}

fn has_component(path: &Path, needle: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case(needle))
    })
}

pub fn discover_default_roots() -> Result<Vec<PathBuf>, CliError> {
    #[cfg(windows)]
    {
        let mut roots = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let path = PathBuf::from(&root);
            if path.is_dir() {
                roots.push(PathBuf::from(root));
            }
        }
        if roots.is_empty() {
            return Err(CliError::Message(
                "no fixed local drives were discovered".to_string(),
            ));
        }
        Ok(roots)
    }

    #[cfg(not(windows))]
    {
        std::env::current_dir()
            .map(|path| vec![path])
            .map_err(|source| CliError::Io {
                context: "failed to discover current directory".to_string(),
                source,
            })
    }
}

pub fn roots_match(requested_roots: &[PathBuf], cached_roots: &[String]) -> bool {
    if requested_roots.len() != cached_roots.len() {
        return false;
    }

    let requested = requested_roots
        .iter()
        .map(|path| normalize_root_for_compare(path))
        .collect::<HashSet<_>>();
    let cached = cached_roots
        .iter()
        .map(PathBuf::from)
        .map(|path| normalize_root_for_compare(&path))
        .collect::<HashSet<_>>();
    requested == cached
}

fn normalize_root_for_compare(path: &Path) -> String {
    let normalized = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    normalized
        .display()
        .to_string()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn total_capacity_for_roots(roots: &[PathBuf]) -> Option<u64> {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut found = false;

    for root in roots {
        let Some((volume, capacity)) = volume_capacity(root) else {
            continue;
        };
        if seen.insert(volume) {
            total = total.saturating_add(capacity);
            found = true;
        }
    }

    found.then_some(total)
}

#[cfg(windows)]
fn volume_capacity(path: &Path) -> Option<(String, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};

    let mut path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut volume = vec![0u16; 260];

    let ok = unsafe {
        GetVolumePathNameW(
            path_wide.as_mut_ptr(),
            volume.as_mut_ptr(),
            volume.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }

    let len = volume
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(volume.len());
    volume.truncate(len + 1);

    let mut total_bytes = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            volume.as_ptr(),
            std::ptr::null_mut(),
            &mut total_bytes,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }

    let volume_name = String::from_utf16_lossy(&volume[..len]).to_ascii_lowercase();
    Some((volume_name, total_bytes))
}

#[cfg(not(windows))]
fn volume_capacity(_path: &Path) -> Option<(String, u64)> {
    None
}

#[cfg(windows)]
fn allocated_size(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, GetFileAttributesW};

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
    if attributes != u32::MAX && cloud_placeholder_attributes(attributes) {
        return Some(0);
    }

    let mut high = 0u32;
    unsafe {
        SetLastError(0);
    }
    let low = unsafe { GetCompressedFileSizeW(path_wide.as_ptr(), &mut high) };
    if low == u32::MAX && unsafe { GetLastError() } != 0 {
        return None;
    }
    Some(((high as u64) << 32) | low as u64)
}

#[cfg(windows)]
fn cloud_placeholder_attributes(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    attributes
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(windows))]
fn allocated_size(_path: &Path) -> Option<u64> {
    None
}

pub fn print_summary(
    snapshot: &Snapshot,
    output: OutputMode,
    limit: usize,
) -> Result<(), CliError> {
    if output.is_json() {
        let categories = snapshot
            .categories
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        print_json(&categories)
    } else {
        println!("Storage summary from unix {}", snapshot.scanned_at_unix);
        println!(
            "Total on disk: {} across {} files",
            human_size(snapshot.total_allocated_bytes),
            snapshot.file_count
        );
        if snapshot.total_allocated_bytes != snapshot.total_bytes {
            println!("Logical size: {}", human_size(snapshot.total_bytes));
        }
        println!(
            "Scanned scope: {}",
            format_scope_percent(
                snapshot.total_allocated_bytes,
                snapshot.total_capacity_bytes
            )
        );
        print_entries(
            snapshot
                .categories
                .iter()
                .take(limit)
                .map(|category| Row {
                    label: category.name.clone(),
                    size_bytes: category.bytes,
                    allocated_bytes: category.allocated_bytes,
                    usage_bytes: category.allocated_bytes,
                    percent: format_percent(
                        category.allocated_bytes,
                        snapshot.total_capacity_bytes,
                    ),
                    detail: format!("{} files", category.files),
                })
                .collect(),
            snapshot.total_allocated_bytes,
            true,
        );
        Ok(())
    }
}

pub fn print_files(snapshot: &Snapshot, output: OutputMode, limit: usize) -> Result<(), CliError> {
    if output.is_json() {
        let entries = snapshot
            .largest_files
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        print_json(&entries)
    } else {
        println!("Files from unix {}", snapshot.scanned_at_unix);
        print_entries(
            snapshot
                .largest_files
                .iter()
                .take(limit)
                .enumerate()
                .map(|(index, entry)| Row {
                    label: format!("{}. {}", index + 1, entry.path),
                    size_bytes: entry.bytes,
                    allocated_bytes: entry.allocated_bytes,
                    usage_bytes: entry.bytes,
                    percent: None,
                    detail: entry.category.clone().unwrap_or_default(),
                })
                .collect(),
            snapshot
                .largest_files
                .first()
                .map(|entry| entry.bytes)
                .unwrap_or(0),
            false,
        );
        Ok(())
    }
}

pub fn print_folders(
    snapshot: &Snapshot,
    output: OutputMode,
    limit: usize,
) -> Result<(), CliError> {
    if output.is_json() {
        let entries = snapshot
            .largest_folders
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        print_json(&entries)
    } else {
        println!("Folders from unix {}", snapshot.scanned_at_unix);
        print_entries(
            snapshot
                .largest_folders
                .iter()
                .take(limit)
                .enumerate()
                .map(|(index, entry)| Row {
                    label: format!("{}. {}", index + 1, entry.path),
                    size_bytes: entry.bytes,
                    allocated_bytes: entry.allocated_bytes,
                    usage_bytes: entry.allocated_bytes,
                    percent: None,
                    detail: entry
                        .files
                        .map(|files| format!("{files} files"))
                        .unwrap_or_default(),
                })
                .collect(),
            snapshot
                .largest_folders
                .first()
                .map(|entry| entry.allocated_bytes)
                .unwrap_or(0),
            false,
        );
        Ok(())
    }
}

pub fn open_cached_path(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    target: NavigationTarget,
    one_based_index: usize,
    limit: usize,
) -> Result<(), CliError> {
    let entry = cached_navigation_entry(snapshot, target, one_based_index, limit)?;
    let navigation = match navigation_for_entry(&entry, target) {
        Ok(navigation) => navigation,
        Err(err) => {
            println!("{err}");
            match offer_prune_missing_entry(paths, snapshot, target, &entry)? {
                PruneOfferResult::Removed | PruneOfferResult::HandledNoChange => return Ok(()),
                PruneOfferResult::NotOffered => return Err(err),
            }
        }
    };
    if matches!(
        offer_prune_missing_entry(paths, snapshot, target, &entry)?,
        PruneOfferResult::Removed
    ) {
        return Ok(());
    }
    launch_file_explorer(&navigation)?;
    println!("{}", navigation.message);
    Ok(())
}

pub fn print_browse(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    output: OutputMode,
    limit: usize,
) -> Result<(), CliError> {
    if output.is_json() {
        return Err(CliError::Message(
            "browse is interactive and does not support --json; use summary, files, or folders"
                .to_string(),
        ));
    }

    loop {
        println!("Cached browse from unix {}", snapshot.scanned_at_unix);
        println!("This view uses cached top-N files and folders only, not a full file inventory.");
        println!("1. Files");
        println!("2. Folders");
        println!("3. Summary categories");
        print!("Choose 1=files, 2=folders, 3=categories, or q=exit: ");
        flush_stdout()?;

        let choice = read_line_trimmed()?.to_ascii_lowercase();
        match choice.as_str() {
            "1" | "files" => browse_files(paths, snapshot, limit)?,
            "2" | "folders" => browse_folders(paths, snapshot, limit)?,
            "3" | "categories" => browse_categories(paths, snapshot, limit)?,
            "" | "q" | "exit" | "quit" => return Ok(()),
            other => {
                return Err(CliError::Message(format!(
                    "unknown browse choice `{other}`"
                )));
            }
        }
    }
}

fn browse_files(paths: &CachePaths, snapshot: &mut Snapshot, limit: usize) -> Result<(), CliError> {
    let files = snapshot
        .largest_files
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    browse_entry_lists(paths, snapshot, "Cached files", &files, &[], limit)
}

fn browse_folders(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    limit: usize,
) -> Result<(), CliError> {
    print_folders(snapshot, OutputMode::Human, limit)?;
    if snapshot.largest_folders.is_empty() {
        return Ok(());
    }

    println!("Folder choices:");
    for (index, entry) in snapshot.largest_folders.iter().take(limit).enumerate() {
        println!(
            "  {}. {} ({})",
            index + 1,
            entry.path,
            human_size(entry.allocated_bytes)
        );
    }
    print!("Folder number to drill into, b=back, or q=exit: ");
    flush_stdout()?;
    let choice = read_line_trimmed()?.to_ascii_lowercase();
    if choice.is_empty() || choice == "b" || choice == "back" {
        return Ok(());
    }
    if is_quit_choice(&choice) {
        std::process::exit(0);
    }
    let index = parse_one_based_index(&choice, snapshot.largest_folders.len().min(limit))?;
    let folder_entry = snapshot.largest_folders[index].clone();
    let folder = folder_entry.path.clone();
    let files = snapshot
        .largest_files
        .iter()
        .filter(|entry| path_is_under(&entry.path, &folder))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let folders = snapshot
        .largest_folders
        .iter()
        .filter(|entry| entry.path != folder && path_is_under(&entry.path, &folder))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    if cached_drilldown_entries_are_empty(&files, &folders) {
        return offer_open_empty_drilldown_folder(paths, snapshot, &folder_entry);
    }
    browse_entry_lists(
        paths,
        snapshot,
        &format!("Cached top-N entries under {folder}"),
        &files,
        &folders,
        limit,
    )
}

fn cached_drilldown_entries_are_empty(files: &[SizedEntry], folders: &[SizedEntry]) -> bool {
    files.is_empty() && folders.is_empty()
}

fn offer_open_empty_drilldown_folder(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    folder_entry: &SizedEntry,
) -> Result<(), CliError> {
    println!("No cached entries under this folder.");
    println!(
        "This can happen when the folder is large but its individual files are below the retained top-N list."
    );

    let navigation = match navigation_for_entry(folder_entry, NavigationTarget::Folder) {
        Ok(navigation) => navigation,
        Err(err) => {
            println!("{err}");
            match offer_prune_missing_entry(
                paths,
                snapshot,
                NavigationTarget::Folder,
                folder_entry,
            )? {
                PruneOfferResult::Removed | PruneOfferResult::HandledNoChange => return Ok(()),
                PruneOfferResult::NotOffered => return Err(err),
            }
        }
    };

    print!("Open this folder in Explorer? [y/N]: ");
    flush_stdout()?;
    let choice = read_line_trimmed()?.to_ascii_lowercase();
    if choice == "y" || choice == "yes" {
        launch_file_explorer(&navigation)?;
        println!("{}", navigation.message);
    }
    Ok(())
}

fn browse_categories(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    limit: usize,
) -> Result<(), CliError> {
    print_summary(snapshot, OutputMode::Human, limit)?;
    if snapshot.categories.is_empty() {
        return Ok(());
    }

    println!("Category choices:");
    for (index, category) in snapshot.categories.iter().take(limit).enumerate() {
        println!(
            "  {}. {} ({})",
            index + 1,
            category.name,
            human_size(category.allocated_bytes)
        );
    }
    print!("Category number to show cached files, b=back, or q=exit: ");
    flush_stdout()?;
    let choice = read_line_trimmed()?.to_ascii_lowercase();
    if choice.is_empty() || choice == "b" || choice == "back" {
        return Ok(());
    }
    if is_quit_choice(&choice) {
        std::process::exit(0);
    }
    let index = parse_one_based_index(&choice, snapshot.categories.len().min(limit))?;
    let category = snapshot.categories[index].name.clone();
    let files = snapshot
        .largest_files
        .iter()
        .filter(|entry| entry.category.as_deref() == Some(category.as_str()))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    browse_entry_lists(
        paths,
        snapshot,
        &format!("Cached top-N files in category {category}"),
        &files,
        &[],
        limit,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Folder,
}

fn browse_entry_lists(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    title: &str,
    files: &[SizedEntry],
    folders: &[SizedEntry],
    limit: usize,
) -> Result<(), CliError> {
    loop {
        println!("{title}");
        if !files.is_empty() {
            println!("Files:");
            print_entry_refs(files, EntryKind::File, limit);
        }
        if !folders.is_empty() {
            println!("Folders:");
            print_entry_refs(folders, EntryKind::Folder, limit);
        }
        if files.is_empty() && folders.is_empty() {
            println!("No cached entries.");
            return Ok(());
        }

        print_detail_menu_prompt(files.is_empty(), folders.is_empty());
        flush_stdout()?;
        let choice = read_line_trimmed()?;
        let normalized = choice.to_ascii_lowercase();
        if normalized.is_empty() {
            return Ok(());
        }
        if is_quit_choice(&normalized) {
            std::process::exit(0);
        }
        if is_detail_back_choice(&normalized, files.is_empty(), folders.is_empty()) {
            return Ok(());
        }

        let Some((kind, index)) =
            parse_detail_menu_choice(&normalized, files.is_empty(), folders.is_empty())?
        else {
            return Err(CliError::Message(format!(
                "unknown details choice `{choice}`"
            )));
        };
        let entry = match kind {
            EntryKind::File => {
                let index = checked_one_based_index(index, files.len(), "file")?;
                files[index].clone()
            }
            EntryKind::Folder => {
                let index = checked_one_based_index(index, folders.len(), "folder")?;
                folders[index].clone()
            }
        };
        if browse_entry_details(paths, snapshot, &entry, kind)? {
            return Ok(());
        }
    }
}

fn print_detail_menu_prompt(files_empty: bool, folders_empty: bool) {
    if files_empty {
        print!("Choose 1=view folder details, 2=back, or q=exit: ");
    } else if folders_empty {
        print!("Choose 1=view file details, 2=back, or q=exit: ");
    } else {
        print!("Choose 1=view file details, 2=view folder details, 3=back, or q=exit: ");
    }
}

fn parse_detail_menu_choice(
    value: &str,
    files_empty: bool,
    folders_empty: bool,
) -> Result<Option<(EntryKind, usize)>, CliError> {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    let action = parts.first().copied().unwrap_or_default();
    if files_empty {
        match action {
            "1" => {
                return Ok(Some((
                    EntryKind::Folder,
                    detail_menu_index(&parts, "folder")?,
                )));
            }
            _ => return Ok(None),
        }
    }
    if folders_empty {
        match action {
            "1" => return Ok(Some((EntryKind::File, detail_menu_index(&parts, "file")?))),
            _ => return Ok(None),
        }
    }
    match action {
        "1" => Ok(Some((EntryKind::File, detail_menu_index(&parts, "file")?))),
        "2" => Ok(Some((
            EntryKind::Folder,
            detail_menu_index(&parts, "folder")?,
        ))),
        _ => Ok(None),
    }
}

fn is_detail_back_choice(value: &str, files_empty: bool, folders_empty: bool) -> bool {
    matches!(value, "" | "b" | "back")
        || if files_empty || folders_empty {
            value == "2"
        } else {
            value == "3"
        }
}

fn detail_menu_index(parts: &[&str], label: &str) -> Result<usize, CliError> {
    if parts.len() >= 2 {
        return parts[1]
            .parse::<usize>()
            .map_err(|_| CliError::Message(format!("{label} number must be a positive integer")));
    }
    print!("Which {label} do you want details for? ");
    flush_stdout()?;
    let choice = read_line_trimmed()?;
    choice
        .parse::<usize>()
        .map_err(|_| CliError::Message(format!("{label} number must be a positive integer")))
}

fn browse_entry_details(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    entry: &SizedEntry,
    kind: EntryKind,
) -> Result<bool, CliError> {
    loop {
        print_entry_details(entry, kind);
        print!("Choose 1=open, 2=back, or q=exit: ");
        flush_stdout()?;
        let choice = read_line_trimmed()?.to_ascii_lowercase();
        match choice.as_str() {
            "1" | "open" => {
                let target = navigation_target(kind);
                let navigation = match navigation_for_entry(entry, target) {
                    Ok(navigation) => navigation,
                    Err(err) => {
                        println!("{err}");
                        match offer_prune_missing_entry(paths, snapshot, target, entry)? {
                            PruneOfferResult::Removed => return Ok(true),
                            PruneOfferResult::HandledNoChange => return Ok(false),
                            PruneOfferResult::NotOffered => {}
                        }
                        if !io::stdin().is_terminal() {
                            return Err(err);
                        } else {
                            return Ok(false);
                        }
                    }
                };
                if matches!(
                    offer_prune_missing_entry(paths, snapshot, target, entry)?,
                    PruneOfferResult::Removed
                ) {
                    return Ok(true);
                }
                launch_file_explorer(&navigation)?;
                println!("{}", navigation.message);
            }
            "" | "2" | "b" | "back" => return Ok(false),
            "q" | "exit" | "quit" => std::process::exit(0),
            other => {
                return Err(CliError::Message(format!(
                    "unknown detail choice `{other}`"
                )));
            }
        }
    }
}

fn print_entry_refs(entries: &[SizedEntry], kind: EntryKind, limit: usize) {
    let max_bytes = entries
        .first()
        .map(|entry| usage_bytes(entry, kind))
        .unwrap_or(0);
    print_entries(
        entries
            .iter()
            .enumerate()
            .take(limit)
            .map(|(index, entry)| {
                let mut row = row_for_entry(entry, kind);
                row.label = format!("{}. {}", index + 1, row.label);
                row
            })
            .collect(),
        max_bytes,
        false,
    );
}

fn row_for_entry(entry: &SizedEntry, kind: EntryKind) -> Row {
    Row {
        label: entry.path.clone(),
        size_bytes: entry.bytes,
        allocated_bytes: entry.allocated_bytes,
        usage_bytes: usage_bytes(entry, kind),
        percent: None,
        detail: match kind {
            EntryKind::File => entry.category.clone().unwrap_or_default(),
            EntryKind::Folder => entry
                .files
                .map(|files| format!("{files} files"))
                .unwrap_or_default(),
        },
    }
}

fn usage_bytes(entry: &SizedEntry, kind: EntryKind) -> u64 {
    match kind {
        EntryKind::File => entry.bytes,
        EntryKind::Folder => entry.allocated_bytes,
    }
}

fn print_entry_details(entry: &SizedEntry, kind: EntryKind) {
    println!("Entry details");
    println!(
        "  type: {}",
        match kind {
            EntryKind::File => "file",
            EntryKind::Folder => "folder",
        }
    );
    println!("  path: {}", entry.path);
    println!("  size: {}", human_size(entry.bytes));
    println!("  on disk: {}", human_size(entry.allocated_bytes));
    match kind {
        EntryKind::File => println!(
            "  category: {}",
            entry.category.as_deref().unwrap_or("unknown")
        ),
        EntryKind::Folder => println!(
            "  files: {}",
            entry
                .files
                .map(|files| files.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    }
    println!(
        "  exists: {}",
        match kind {
            EntryKind::File => Path::new(&entry.path).is_file(),
            EntryKind::Folder => Path::new(&entry.path).is_dir(),
        }
    );
}

fn navigation_target(kind: EntryKind) -> NavigationTarget {
    match kind {
        EntryKind::File => NavigationTarget::File,
        EntryKind::Folder => NavigationTarget::Folder,
    }
}

fn is_quit_choice(value: &str) -> bool {
    matches!(value, "q" | "exit" | "quit")
}

fn parse_one_based_index(value: &str, len: usize) -> Result<usize, CliError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| CliError::Message("selection must be a number".to_string()))?;
    if parsed == 0 || parsed > len {
        return Err(CliError::Message(format!(
            "selection must be between 1 and {len}"
        )));
    }
    Ok(parsed - 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NavigationPath {
    path: PathBuf,
    select_file: bool,
    message: String,
}

#[cfg(test)]
fn resolve_navigation_path(
    snapshot: &Snapshot,
    target: NavigationTarget,
    one_based_index: usize,
    limit: usize,
) -> Result<NavigationPath, CliError> {
    let entry = cached_navigation_entry(snapshot, target, one_based_index, limit)?;
    navigation_for_entry(&entry, target)
}

fn cached_navigation_entry(
    snapshot: &Snapshot,
    target: NavigationTarget,
    one_based_index: usize,
    limit: usize,
) -> Result<SizedEntry, CliError> {
    match target {
        NavigationTarget::File => {
            let index = checked_one_based_index(
                one_based_index,
                snapshot.largest_files.len().min(limit),
                "file",
            )?;
            Ok(snapshot.largest_files[index].clone())
        }
        NavigationTarget::Folder => {
            let index = checked_one_based_index(
                one_based_index,
                snapshot.largest_folders.len().min(limit),
                "folder",
            )?;
            Ok(snapshot.largest_folders[index].clone())
        }
    }
}

fn navigation_for_entry(
    entry: &SizedEntry,
    target: NavigationTarget,
) -> Result<NavigationPath, CliError> {
    match target {
        NavigationTarget::File => {
            let file = PathBuf::from(&entry.path);
            let parent = file.parent().ok_or_else(|| {
                CliError::Message(format!(
                    "cached file has no containing folder: {}",
                    file.display()
                ))
            })?;
            if file.is_file() {
                Ok(NavigationPath {
                    path: file.clone(),
                    select_file: true,
                    message: format!("Opening Explorer and selecting {}", file.display()),
                })
            } else if parent.is_dir() {
                Ok(NavigationPath {
                    path: parent.to_path_buf(),
                    select_file: false,
                    message: format!(
                        "Opening containing folder {}; cached file was not found at {}",
                        parent.display(),
                        file.display()
                    ),
                })
            } else {
                Err(CliError::Message(format!(
                    "cached file and containing folder no longer exist: {}",
                    file.display()
                )))
            }
        }
        NavigationTarget::Folder => {
            let folder = PathBuf::from(&entry.path);
            if !folder.is_dir() {
                return Err(CliError::Message(format!(
                    "cached folder no longer exists: {}",
                    folder.display()
                )));
            }
            Ok(NavigationPath {
                path: folder.clone(),
                select_file: false,
                message: format!("Opening Explorer at {}", folder.display()),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PruneOfferResult {
    NotOffered,
    HandledNoChange,
    Removed,
}

fn offer_prune_missing_entry(
    paths: &CachePaths,
    snapshot: &mut Snapshot,
    target: NavigationTarget,
    entry: &SizedEntry,
) -> Result<PruneOfferResult, CliError> {
    if !io::stdin().is_terminal() || !entry_is_missing(entry, target) {
        return Ok(PruneOfferResult::NotOffered);
    }

    println!();
    println!(
        "Missing cached {} detected.",
        match target {
            NavigationTarget::File => "file",
            NavigationTarget::Folder => "folder",
        }
    );
    println!("Warning: this only edits the saved dscan11 snapshot.");
    println!("It does not delete anything from disk and it does not rescan the drive.");
    if matches!(target, NavigationTarget::Folder) {
        println!("Folder category totals may stay approximate until the next full scan.");
    }
    print!(
        "Remove this missing {} from the scan cache? [y/N]: ",
        match target {
            NavigationTarget::File => "file",
            NavigationTarget::Folder => "folder",
        }
    );
    flush_stdout()?;

    let choice = read_line_trimmed()?.to_ascii_lowercase();
    if choice != "y" && choice != "yes" {
        println!("Cache unchanged.");
        return Ok(PruneOfferResult::HandledNoChange);
    }

    let result = prune_cached_entry(snapshot, target, entry);
    if result.removed_files == 0 && result.removed_folders == 0 {
        println!("No matching cached entry was found; cache unchanged.");
        return Ok(PruneOfferResult::HandledNoChange);
    }

    append_cleanup_journal(paths, target, entry, &result)?;
    save_snapshot(paths, snapshot)?;
    println!(
        "Tracked removal of {} file(s) and {} folder(s).",
        result.removed_files, result.removed_folders
    );
    println!(
        "Saved updated cache snapshot: {}",
        paths.snapshot_path.display()
    );
    Ok(PruneOfferResult::Removed)
}

fn cached_entry_exists(entry: &SizedEntry, target: NavigationTarget) -> bool {
    match target {
        NavigationTarget::File => Path::new(&entry.path).is_file(),
        NavigationTarget::Folder => Path::new(&entry.path).is_dir(),
    }
}

fn entry_is_missing(entry: &SizedEntry, target: NavigationTarget) -> bool {
    !cached_entry_exists(entry, target)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PruneResult {
    removed_files: usize,
    removed_folders: usize,
}

fn prune_cached_entry(
    snapshot: &mut Snapshot,
    target: NavigationTarget,
    entry: &SizedEntry,
) -> PruneResult {
    match target {
        NavigationTarget::File => prune_cached_file(snapshot, entry),
        NavigationTarget::Folder => prune_cached_folder(snapshot, entry),
    }
}

fn prune_cached_file(snapshot: &mut Snapshot, entry: &SizedEntry) -> PruneResult {
    let before = snapshot.largest_files.len();
    snapshot
        .largest_files
        .retain(|cached| !same_path(&cached.path, &entry.path));
    let removed_files = before - snapshot.largest_files.len();
    if removed_files > 0 {
        subtract_snapshot_file(snapshot, entry);
    }
    PruneResult {
        removed_files,
        removed_folders: 0,
    }
}

fn prune_cached_folder(snapshot: &mut Snapshot, entry: &SizedEntry) -> PruneResult {
    let removed_file_entries = snapshot
        .largest_files
        .iter()
        .filter(|cached| path_is_under(&cached.path, &entry.path))
        .cloned()
        .collect::<Vec<_>>();
    let removed_file_entries_len = removed_file_entries.len();

    snapshot
        .largest_files
        .retain(|cached| !path_is_under(&cached.path, &entry.path));

    let before_folders = snapshot.largest_folders.len();
    snapshot
        .largest_folders
        .retain(|cached| !path_is_under(&cached.path, &entry.path));
    let removed_folders = before_folders - snapshot.largest_folders.len();

    if removed_folders > 0 {
        snapshot.total_bytes = snapshot.total_bytes.saturating_sub(entry.bytes);
        snapshot.total_allocated_bytes = snapshot
            .total_allocated_bytes
            .saturating_sub(entry.allocated_bytes);
        let removed_file_count = entry.files.unwrap_or(removed_file_entries_len as u64);
        snapshot.file_count = snapshot.file_count.saturating_sub(removed_file_count);
        snapshot.folder_count = snapshot.folder_count.saturating_sub(removed_folders as u64);
        subtract_folder_from_cached_ancestors(snapshot, entry);
        let mut preferred_categories = Vec::new();
        for file in &removed_file_entries {
            if let Some(category) = subtract_category_file(snapshot, file) {
                preferred_categories.push(category);
            }
        }
        reconcile_category_totals(snapshot, &preferred_categories);
    }

    PruneResult {
        removed_files: entry.files.unwrap_or(removed_file_entries_len as u64) as usize,
        removed_folders,
    }
}

fn subtract_snapshot_file(snapshot: &mut Snapshot, entry: &SizedEntry) {
    snapshot.total_bytes = snapshot.total_bytes.saturating_sub(entry.bytes);
    snapshot.total_allocated_bytes = snapshot
        .total_allocated_bytes
        .saturating_sub(entry.allocated_bytes);
    snapshot.file_count = snapshot.file_count.saturating_sub(1);
    let preferred_categories = subtract_category_file(snapshot, entry)
        .into_iter()
        .collect::<Vec<_>>();
    reconcile_category_totals(snapshot, &preferred_categories);
}

fn subtract_folder_from_cached_ancestors(snapshot: &mut Snapshot, entry: &SizedEntry) {
    let mut changed = false;
    for cached in &mut snapshot.largest_folders {
        if !path_is_under(&entry.path, &cached.path) {
            continue;
        }
        cached.bytes = cached.bytes.saturating_sub(entry.bytes);
        cached.allocated_bytes = cached.allocated_bytes.saturating_sub(entry.allocated_bytes);
        if let (Some(cached_files), Some(removed_files)) = (cached.files, entry.files) {
            cached.files = Some(cached_files.saturating_sub(removed_files));
        }
        changed = true;
    }
    if changed {
        sort_largest_folders(&mut snapshot.largest_folders);
    }
}

fn sort_largest_folders(largest_folders: &mut [SizedEntry]) {
    largest_folders.sort_by(|a, b| {
        b.allocated_bytes
            .cmp(&a.allocated_bytes)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn subtract_category_file(snapshot: &mut Snapshot, entry: &SizedEntry) -> Option<String> {
    let Some(category_name) = entry.category.as_deref() else {
        return None;
    };
    if let Some(category) = snapshot
        .categories
        .iter_mut()
        .find(|category| category.name == category_name)
    {
        category.bytes = category.bytes.saturating_sub(entry.bytes);
        category.allocated_bytes = category
            .allocated_bytes
            .saturating_sub(entry.allocated_bytes);
        category.files = category.files.saturating_sub(1);
    }
    snapshot.categories.retain(|category| {
        category.bytes > 0 || category.allocated_bytes > 0 || category.files > 0
    });
    Some(category_name.to_string())
}

fn reconcile_category_totals(snapshot: &mut Snapshot, preferred_categories: &[String]) {
    let category_bytes = snapshot
        .categories
        .iter()
        .map(|category| category.bytes)
        .sum::<u64>();
    let category_allocated_bytes = snapshot
        .categories
        .iter()
        .map(|category| category.allocated_bytes)
        .sum::<u64>();
    let category_files = snapshot
        .categories
        .iter()
        .map(|category| category.files)
        .sum::<u64>();

    subtract_category_excess(
        &mut snapshot.categories,
        category_bytes.saturating_sub(snapshot.total_bytes),
        category_allocated_bytes.saturating_sub(snapshot.total_allocated_bytes),
        category_files.saturating_sub(snapshot.file_count),
        preferred_categories,
    );
    sort_categories(&mut snapshot.categories);
}

fn subtract_category_excess(
    categories: &mut Vec<CategoryTotal>,
    mut bytes: u64,
    mut allocated_bytes: u64,
    mut files: u64,
    preferred_categories: &[String],
) {
    for preferred in preferred_categories {
        let Some(category) = categories
            .iter_mut()
            .find(|category| category.name == *preferred)
        else {
            continue;
        };
        subtract_from_category(category, &mut bytes, &mut allocated_bytes, &mut files);
        if bytes == 0 && allocated_bytes == 0 && files == 0 {
            break;
        }
    }

    for category in categories.iter_mut() {
        subtract_from_category(category, &mut bytes, &mut allocated_bytes, &mut files);
        if bytes == 0 && allocated_bytes == 0 && files == 0 {
            break;
        }
    }
    categories.retain(|category| {
        category.bytes > 0 || category.allocated_bytes > 0 || category.files > 0
    });
}

fn subtract_from_category(
    category: &mut CategoryTotal,
    bytes: &mut u64,
    allocated_bytes: &mut u64,
    files: &mut u64,
) {
    if *bytes > 0 {
        let subtract = category.bytes.min(*bytes);
        category.bytes -= subtract;
        *bytes -= subtract;
    }
    if *allocated_bytes > 0 {
        let subtract = category.allocated_bytes.min(*allocated_bytes);
        category.allocated_bytes -= subtract;
        *allocated_bytes -= subtract;
    }
    if *files > 0 {
        let subtract = category.files.min(*files);
        category.files -= subtract;
        *files -= subtract;
    }
}

fn sort_categories(categories: &mut [CategoryTotal]) {
    categories.sort_by(|a, b| {
        b.allocated_bytes
            .cmp(&a.allocated_bytes)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn same_path(left: &str, right: &str) -> bool {
    normalized_path(left) == normalized_path(right)
}

fn normalized_path(path: &str) -> String {
    path.trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn checked_one_based_index(
    one_based_index: usize,
    len: usize,
    label: &str,
) -> Result<usize, CliError> {
    if len == 0 {
        return Err(CliError::Message(format!("no cached {label} entries")));
    }
    if one_based_index == 0 || one_based_index > len {
        return Err(CliError::Message(format!(
            "{label} number must be between 1 and {len}"
        )));
    }
    Ok(one_based_index - 1)
}

#[cfg(windows)]
fn launch_file_explorer(navigation: &NavigationPath) -> Result<(), CliError> {
    let mut command = Command::new("explorer.exe");
    for arg in explorer_args_for_navigation(navigation) {
        command.arg(arg);
    }
    command.spawn().map(|_| ()).map_err(|source| CliError::Io {
        context: "failed to open Explorer".to_string(),
        source,
    })
}

fn explorer_args_for_navigation(navigation: &NavigationPath) -> Vec<String> {
    if navigation.select_file {
        vec![format!("/select,\"{}\"", navigation.path.display())]
    } else {
        vec![format!("/e,\"{}\"", navigation.path.display())]
    }
}

#[cfg(not(windows))]
fn launch_file_explorer(navigation: &NavigationPath) -> Result<(), CliError> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let path = if navigation.select_file {
        navigation.path.parent().unwrap_or(&navigation.path)
    } else {
        &navigation.path
    };
    Command::new(opener)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|source| CliError::Io {
            context: format!("failed to open file browser with {opener}"),
            source,
        })
}

fn path_is_under(path: &str, folder: &str) -> bool {
    let path = normalized_path(path);
    let folder = normalized_path(folder);
    path == folder || path.starts_with(&format!("{folder}\\"))
}

fn read_line_trimmed() -> Result<String, CliError> {
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|source| CliError::Io {
            context: "failed to read selection".to_string(),
            source,
        })?;
    Ok(value.trim().to_string())
}

fn flush_stdout() -> Result<(), CliError> {
    use std::io::Write;

    io::stdout().flush().map_err(|source| CliError::Io {
        context: "failed to flush stdout".to_string(),
        source,
    })
}

pub fn print_status(
    snapshot: &Snapshot,
    paths: &CachePaths,
    config: &AppConfig,
    category_rules: Option<&CategoryRules>,
    output: OutputMode,
) -> Result<(), CliError> {
    #[derive(Serialize)]
    struct Status<'a> {
        app_version: &'static str,
        snapshot_schema_version: u32,
        scanned_at_unix: u64,
        scanned_at_utc: String,
        stale_at_utc: Option<String>,
        stale: StaleInfo,
        workspace: String,
        cache_mode: CacheMode,
        roots: &'a [String],
        cache_dir: String,
        snapshot_path: String,
        base_snapshot_path: String,
        total_bytes: u64,
        total_allocated_bytes: u64,
        total_capacity_bytes: Option<u64>,
        scope_drive_percent: Option<f64>,
        file_count: u64,
        folder_count: u64,
        skipped_count: usize,
        access_denied_count: u64,
        category_rules: Option<CategoryRulesStatus>,
        scan_stats: &'a ScanStats,
        manual_cleanups: ManualCleanupTotals,
        cache_savings: CacheSavingsTotals,
    }

    let category_rules = category_rules.map(|rules| category_rules_status(snapshot, rules));
    let manual_cleanups = manual_cleanup_totals(paths)?;
    let cache_savings = cache_savings_totals(paths)?;
    let scanned_at_utc = format_unix_utc(snapshot.scanned_at_unix)?;
    let stale_at_utc = if config.stale_days == 0 {
        None
    } else {
        Some(format_unix_utc(
            snapshot
                .scanned_at_unix
                .saturating_add(config.stale_days.saturating_mul(86_400)),
        )?)
    };
    let status = Status {
        app_version: APP_VERSION,
        snapshot_schema_version: SNAPSHOT_VERSION,
        scanned_at_unix: snapshot.scanned_at_unix,
        scanned_at_utc,
        stale_at_utc,
        stale: snapshot.stale_info(config.stale_days),
        workspace: paths.workspace_name.clone(),
        cache_mode: cache_mode(paths, snapshot),
        roots: &snapshot.roots,
        cache_dir: paths.base_dir.display().to_string(),
        snapshot_path: paths.snapshot_path.display().to_string(),
        base_snapshot_path: paths.base_snapshot_path.display().to_string(),
        total_bytes: snapshot.total_bytes,
        total_allocated_bytes: snapshot.total_allocated_bytes,
        total_capacity_bytes: snapshot.total_capacity_bytes,
        scope_drive_percent: percent_value(
            snapshot.total_allocated_bytes,
            snapshot.total_capacity_bytes,
        ),
        file_count: snapshot.file_count,
        folder_count: snapshot.folder_count,
        skipped_count: snapshot.skipped.len(),
        access_denied_count: snapshot.access_denied_count,
        category_rules,
        scan_stats: &snapshot.scan_stats,
        manual_cleanups,
        cache_savings,
    };

    if output.is_json() {
        print_json(&status)
    } else {
        println!("Scan status");
        println!("Version");
        println!("  app: {}", status.app_version);
        println!("  snapshot schema: {}", status.snapshot_schema_version);

        println!("Scan freshness");
        println!("  scanned unix: {}", status.scanned_at_unix);
        println!("  scanned at: {}", status.scanned_at_utc);
        println!(
            "  age: {} days{}",
            status.stale.age_days,
            if status.stale.is_stale {
                " (stale)"
            } else {
                ""
            }
        );
        println!("  stale after: {} days", status.stale.stale_after_days);
        println!(
            "  stale on: {}",
            status
                .stale_at_utc
                .as_deref()
                .unwrap_or("disabled by stale-days 0")
        );

        println!("Cache");
        println!("  workspace: {}", status.workspace);
        println!(
            "  cache mode: {}",
            match status.cache_mode {
                CacheMode::BaseScan => "base scan",
                CacheMode::PresentTracked => "present as tracked",
            }
        );
        println!("  cache: {}", status.cache_dir);
        println!("  snapshot: {}", status.snapshot_path);
        println!("  base snapshot: {}", status.base_snapshot_path);
        println!("  roots: {}", status.roots.join(", "));

        println!("Drive scope");
        println!(
            "  total on disk: {}",
            human_size(status.total_allocated_bytes)
        );
        if status.total_allocated_bytes != status.total_bytes {
            println!("  logical size: {}", human_size(status.total_bytes));
        }
        println!(
            "  drive capacity: {}",
            status
                .total_capacity_bytes
                .map(human_size)
                .unwrap_or_else(|| "n/a".to_string())
        );
        println!(
            "  scope of drive: {}",
            status
                .scope_drive_percent
                .map(|percent| format!("{percent:.1}%"))
                .unwrap_or_else(|| "n/a".to_string())
        );

        println!("Inventory");
        println!("  files: {}", status.file_count);
        println!("  folders: {}", status.folder_count);
        println!("  skipped: {}", status.skipped_count);
        println!("  access denied: {}", status.access_denied_count);

        println!("Manual cleanups");
        println!(
            "  manual cleanups: {} event(s), {} file(s), {} folder(s), {} on disk",
            status.manual_cleanups.events,
            status.manual_cleanups.removed_files,
            status.manual_cleanups.removed_folders,
            status.manual_cleanups.human_on_disk
        );
        if status.manual_cleanups.allocated_bytes != status.manual_cleanups.bytes {
            println!(
                "  manual cleanup logical size: {}",
                status.manual_cleanups.human_size
            );
        }

        println!("Cache savings");
        println!(
            "  avoided readouts: {}",
            status.cache_savings.counted_readouts
        );
        println!(
            "  estimated scan work avoided: {} on disk, {} file checks, {} folder checks, {}",
            status.cache_savings.estimated_on_disk_not_rewalked,
            status.cache_savings.estimated_files_not_rechecked,
            status.cache_savings.estimated_folders_not_rechecked,
            status.cache_savings.estimated_scan_time_saved
        );
        println!(
            "  cache navigations: {}",
            status.cache_savings.cache_navigation_count
        );
        println!("  savings basis: {}", status.cache_savings.basis);
        if let Some(category_rules) = &status.category_rules {
            println!("Category rules");
            let message = match category_rules.changed_since_scan {
                Some(false) => "unchanged".to_string(),
                Some(true) => {
                    "changed since scan; run dscan11 scan to refresh category totals".to_string()
                }
                None => "unknown; run dscan11 scan to record category rules".to_string(),
            };
            println!("  category rules: {message}");
        }

        println!("Initial Scan Performance");
        println!(
            "  elapsed: {}",
            human_duration(status.scan_stats.elapsed_ms)
        );
        println!(
            "  effective average scan rate: {}",
            human_rate(status.scan_stats.allocated_bytes_per_second)
        );
        println!(
            "  effective average logical rate: {}",
            human_rate(status.scan_stats.logical_bytes_per_second)
        );
        println!(
            "  file rate: {}/s",
            human_count_rate(status.scan_stats.files_per_second)
        );
        println!(
            "  folder rate: {}/s",
            human_count_rate(status.scan_stats.folders_per_second)
        );
        println!("  Rates are effective averages for this scan, not raw disk benchmark results.");
        Ok(())
    }
}

pub fn print_config(
    config: &AppConfig,
    paths: &CachePaths,
    output: OutputMode,
) -> Result<(), CliError> {
    #[derive(Serialize)]
    struct ConfigView<'a> {
        app_version: &'static str,
        config_path: String,
        category_config_path: String,
        workspace_registry_path: String,
        supported_config_version: u32,
        supported_category_config_version: u32,
        supported_workspace_registry_version: u32,
        config_version: u32,
        category_config_version: Option<u32>,
        workspace_registry_version: Option<u32>,
        config: &'a AppConfig,
    }

    let view = ConfigView {
        app_version: APP_VERSION,
        config_path: paths.config_path.display().to_string(),
        category_config_path: paths.category_config_path.display().to_string(),
        workspace_registry_path: paths.workspace_registry_path.display().to_string(),
        supported_config_version: CONFIG_VERSION,
        supported_category_config_version: CATEGORY_CONFIG_VERSION,
        supported_workspace_registry_version: WORKSPACE_REGISTRY_VERSION,
        config_version: config.version,
        category_config_version: read_json_version(
            &paths.category_config_path,
            CATEGORY_CONFIG_VERSION,
        )?,
        workspace_registry_version: read_json_version(
            &paths.workspace_registry_path,
            WORKSPACE_REGISTRY_VERSION,
        )?,
        config,
    };

    if output.is_json() {
        print_json(&view)
    } else {
        println!("Config");
        println!("  app version: {}", view.app_version);
        println!("  path: {}", view.config_path);
        println!("  category config: {}", view.category_config_path);
        println!("  workspace registry: {}", view.workspace_registry_path);
        println!("  config schema: {}", view.config_version);
        println!(
            "  category config schema: {}",
            view.category_config_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "missing".to_string())
        );
        println!(
            "  workspace registry schema: {}",
            view.workspace_registry_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "missing".to_string())
        );
        println!("  stale days: {}", config.stale_days);
        println!("  top limit: {}", config.top_limit);
        println!("  skip names: {}", config.skip_names.join(", "));
        Ok(())
    }
}

fn read_json_version(path: &Path, legacy_version: u32) -> Result<Option<u32>, CliError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CliError::Io {
                context: format!("failed to read {}", path.display()),
                source,
            });
        }
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Ok(None);
    };
    Ok(Some(
        value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .unwrap_or(legacy_version),
    ))
}

pub fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<(), CliError> {
    let json = serde_json::to_string_pretty(value).map_err(|source| CliError::Json {
        context: "failed to serialize JSON output".to_string(),
        source,
    })?;
    println!("{json}");
    Ok(())
}

#[derive(Debug)]
struct Row {
    label: String,
    size_bytes: u64,
    allocated_bytes: u64,
    usage_bytes: u64,
    percent: Option<String>,
    detail: String,
}

fn print_entries(rows: Vec<Row>, max_bytes: u64, show_percent: bool) {
    if rows.is_empty() {
        println!("No cached entries.");
        return;
    }

    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .max(80);
    let bar_width = 24usize.min(width / 3).max(12);
    let size_width = 10;
    let allocated_width = 10;
    let percent_width = if show_percent { 8 } else { 0 };
    let detail_width = 18;
    let fixed = bar_width
        + size_width
        + allocated_width
        + percent_width
        + detail_width
        + if show_percent { 12 } else { 10 };
    let label_width = width.saturating_sub(fixed).max(24);

    if show_percent {
        println!(
            "{:<label_width$} {:>size_width$}  {:>allocated_width$}  {:>percent_width$}  {:<bar_width$}  {:<detail_width$}",
            "Name",
            "Size",
            "On Disk",
            "% Drive",
            "Usage",
            "Details",
            label_width = label_width,
            size_width = size_width,
            allocated_width = allocated_width,
            percent_width = percent_width,
            bar_width = bar_width,
            detail_width = detail_width,
        );
    } else {
        println!(
            "{:<label_width$} {:>size_width$}  {:>allocated_width$}  {:<bar_width$}  {:<detail_width$}",
            "Name",
            "Size",
            "On Disk",
            "Usage",
            "Details",
            label_width = label_width,
            size_width = size_width,
            allocated_width = allocated_width,
            bar_width = bar_width,
            detail_width = detail_width,
        );
    }

    for row in rows {
        let ratio = if max_bytes == 0 {
            0.0
        } else {
            row.usage_bytes as f64 / max_bytes as f64
        };
        let filled = ((ratio * bar_width as f64).round() as usize).min(bar_width);
        let bar = format!("{}{}", "#".repeat(filled), "-".repeat(bar_width - filled));
        if show_percent {
            println!(
                "{:<label_width$} {:>size_width$}  {:>allocated_width$}  {:>percent_width$}  {}  {:<detail_width$}",
                truncate_middle(&row.label, label_width),
                human_size(row.size_bytes),
                human_size(row.allocated_bytes),
                row.percent.unwrap_or_else(|| "n/a".to_string()),
                bar,
                truncate_end(&row.detail, detail_width),
                label_width = label_width,
                size_width = size_width,
                allocated_width = allocated_width,
                percent_width = percent_width,
                detail_width = detail_width,
            );
        } else {
            println!(
                "{:<label_width$} {:>size_width$}  {:>allocated_width$}  {}  {:<detail_width$}",
                truncate_middle(&row.label, label_width),
                human_size(row.size_bytes),
                human_size(row.allocated_bytes),
                bar,
                truncate_end(&row.detail, detail_width),
                label_width = label_width,
                size_width = size_width,
                allocated_width = allocated_width,
                detail_width = detail_width,
            );
        }
    }
}

fn format_scope_percent(bytes: u64, capacity: Option<u64>) -> String {
    match capacity {
        Some(capacity) if capacity > 0 => {
            let percent = bytes as f64 / capacity as f64 * 100.0;
            format!(
                "{} / {} ({percent:.1}%)",
                human_size(bytes),
                human_size(capacity)
            )
        }
        _ => format!("{} / n/a (n/a)", human_size(bytes)),
    }
}

fn format_percent(bytes: u64, capacity: Option<u64>) -> Option<String> {
    percent_value(bytes, capacity).map(|percent| format!("{percent:.1}%"))
}

fn percent_value(bytes: u64, capacity: Option<u64>) -> Option<f64> {
    let capacity = capacity?;
    if capacity == 0 {
        return None;
    }
    Some(bytes as f64 / capacity as f64 * 100.0)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn human_rate(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", human_size(bytes_per_second.round() as u64))
}

fn human_count_rate(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "0".to_string();
    }
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn human_duration(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let millis = ms % 1_000;
    if total_seconds < 1 {
        return format!("{millis}ms");
    }

    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds}.{tenths}s", tenths = millis / 100)
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let keep = max_chars - 3;
    let left_count = keep / 2;
    let right_count = keep - left_count;
    let left = value.chars().take(left_count).collect::<String>();
    let right = value
        .chars()
        .rev()
        .take(right_count)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{left}...{right}")
}

fn truncate_end(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    format!(
        "{}...",
        value.chars().take(max_chars - 3).collect::<String>()
    )
}

fn current_unix() -> Result<u64, CliError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| CliError::Message(format!("system clock is before Unix epoch: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_extensions_and_paths() {
        assert_eq!(classify_path(Path::new(r"C:\x\movie.mkv")), "Videos");
        assert_eq!(classify_path(Path::new(r"C:\x\archive.7z")), "Archives");
        assert_eq!(
            classify_path(Path::new(r"C:\x\disk.vhdx")),
            "Disk Images / VMs"
        );
        assert_eq!(
            classify_path(Path::new(r"C:\Users\Example\OneDrive\thing.bin")),
            "Cloud / OneDrive"
        );
        assert_eq!(
            classify_path(Path::new(r"C:\work\project\target\debug\a.obj")),
            "Developer / Code"
        );
        assert_eq!(
            classify_path(Path::new(
                r"C:\Users\Example\.ollama\models\blobs\sha256-acaad28d51b81c74"
            )),
            "AI Models"
        );
        assert_eq!(
            classify_path(Path::new(
                r"C:\Users\Example\AppData\Local\Docker\wsl\data\ext4.vhdx"
            )),
            "Docker / Containers"
        );
    }

    #[test]
    fn default_category_rules_match_public_classifier() {
        let rules = CategoryRules::from_config(CategoryConfig::default(), "builtin".to_string())
            .expect("default rules");

        for path in [
            r"C:\x\movie.mkv",
            r"C:\x\archive.7z",
            r"C:\x\disk.vhdx",
            r"C:\Users\Example\OneDrive\thing.bin",
            r"C:\work\project\target\debug\a.obj",
            r"C:\Users\Example\.ollama\models\blobs\sha256-acaad28d51b81c74",
            r"C:\Users\Example\AppData\Local\Docker\wsl\data\ext4.vhdx",
        ] {
            assert_eq!(
                rules.classify(Path::new(path)),
                classify_path(Path::new(path))
            );
        }
    }

    #[test]
    fn path_rules_take_priority_over_extensions_and_cloud_fallback() {
        assert_eq!(
            classify_path(Path::new(
                r"C:\Users\Example\AppData\Local\Docker\wsl\data\ext4.vhdx"
            )),
            "Docker / Containers"
        );
        assert_eq!(
            classify_path(Path::new(r"C:\Users\Example\.ollama\models\weights.pdf")),
            "AI Models"
        );
        assert_eq!(
            classify_path(Path::new(r"C:\Users\Example\OneDrive\notes.pdf")),
            "Documents"
        );
        assert_eq!(
            classify_path(Path::new(r"C:\Users\Example\OneDrive\thing.unknown")),
            "Cloud / OneDrive"
        );
    }

    #[test]
    fn classifies_common_ai_model_and_docker_storage_paths() {
        for path in [
            r"C:\Users\Example\.cache\huggingface\hub\models--org--model\blobs\abc",
            r"C:\Users\Example\huggingface\hub\models--org--model\blobs\abc",
            r"C:\Users\Example\AppData\Local\LM Studio\models\publisher\model.gguf",
            r"C:\Users\Example\AppData\Local\GPT4All\model.gguf",
        ] {
            assert_eq!(classify_path(Path::new(path)), "AI Models");
        }

        for path in [
            r"C:\ProgramData\docker\windowsfilter\layer\file",
            r"C:\ProgramData\docker\containers\id\config.v2.json",
            r"C:\ProgramData\docker\volumes\volume\_data\blob",
            r"C:\Users\Example\.docker\desktop\vm-data\DockerDesktop.vhdx",
        ] {
            assert_eq!(classify_path(Path::new(path)), "Docker / Containers");
        }
    }

    #[test]
    fn category_fingerprint_is_stable_for_reordered_extensions() {
        let mut first = BTreeMap::new();
        first.insert(
            "Documents".to_string(),
            vec!["TXT".to_string(), ".pdf".to_string()],
        );
        let mut second = BTreeMap::new();
        second.insert(
            "Documents".to_string(),
            vec!["pdf".to_string(), "txt".to_string()],
        );

        let first = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: first,
                path_rules: None,
            },
            "test".to_string(),
        )
        .expect("first rules");
        let second = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: second,
                path_rules: None,
            },
            "test".to_string(),
        )
        .expect("second rules");

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn category_fingerprint_changes_when_mapping_changes() {
        let mut first = BTreeMap::new();
        first.insert("Documents".to_string(), vec!["pdf".to_string()]);
        let mut second = BTreeMap::new();
        second.insert("Archives".to_string(), vec!["pdf".to_string()]);

        let first = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: first,
                path_rules: None,
            },
            "test".to_string(),
        )
        .expect("first rules");
        let second = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: second,
                path_rules: None,
            },
            "test".to_string(),
        )
        .expect("second rules");

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn category_fingerprint_changes_when_path_rules_change() {
        let mut first_path_rules = BTreeMap::new();
        first_path_rules.insert("AI Models".to_string(), vec![".ollama/models".to_string()]);
        let mut second_path_rules = BTreeMap::new();
        second_path_rules.insert(
            "Docker / Containers".to_string(),
            vec![".ollama/models".to_string()],
        );

        let first = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: BTreeMap::new(),
                path_rules: Some(first_path_rules),
            },
            "test".to_string(),
        )
        .expect("first rules");
        let second = CategoryRules::from_config(
            CategoryConfig {
                version: CATEGORY_CONFIG_VERSION,
                categories: BTreeMap::new(),
                path_rules: Some(second_path_rules),
            },
            "test".to_string(),
        )
        .expect("second rules");

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn old_style_category_config_without_path_rules_still_loads() {
        let config: CategoryConfig =
            serde_json::from_str(r#"{"categories":{"Documents":["pdf"]}}"#)
                .expect("old category config parses");
        let rules = CategoryRules::from_config(config, "test".to_string()).expect("rules");

        assert_eq!(
            rules.classify(Path::new(
                r"C:\Users\Example\.ollama\models\blobs\sha256-abc",
            )),
            "AI Models"
        );
    }

    #[test]
    fn stale_info_uses_configurable_threshold() {
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time") - (16 * 86_400),
            roots: vec!["C:\\".to_string()],
            total_bytes: 0,
            total_allocated_bytes: 0,
            total_capacity_bytes: None,
            file_count: 0,
            folder_count: 0,
            categories: vec![],
            largest_files: vec![],
            largest_folders: vec![],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: None,
            category_rules_source: None,
            scan_stats: ScanStats::default(),
        };

        assert!(snapshot.stale_info(15).is_stale);
        assert!(!snapshot.stale_info(30).is_stale);
    }

    #[test]
    fn formats_drive_percentages() {
        assert_eq!(format_percent(50, Some(200)).as_deref(), Some("25.0%"));
        assert_eq!(format_percent(1, Some(3)).as_deref(), Some("33.3%"));
        assert_eq!(format_percent(50, Some(0)), None);
        assert_eq!(format_percent(50, None), None);
        assert_eq!(format_scope_percent(50, Some(200)), "50 B / 200 B (25.0%)");
        assert_eq!(format_scope_percent(50, None), "50 B / n/a (n/a)");
    }

    #[test]
    fn root_matching_is_order_independent() {
        let dir = temp_dir("root_matching");
        let child = dir.join("child");
        fs::create_dir_all(&child).expect("mkdir");

        assert!(roots_match(
            &[child.clone(), dir.clone()],
            &[dir.display().to_string(), child.display().to_string()]
        ));
        assert!(!roots_match(
            std::slice::from_ref(&dir),
            &[dir.display().to_string(), child.display().to_string()]
        ));
    }

    #[test]
    fn cache_roundtrip_preserves_snapshot() {
        let dir = temp_dir("cache_roundtrip");
        let paths = test_cache_paths(&dir);
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time"),
            roots: vec![dir.display().to_string()],
            total_bytes: 42,
            total_allocated_bytes: 42,
            total_capacity_bytes: Some(100),
            file_count: 1,
            folder_count: 1,
            categories: vec![CategoryTotal {
                name: "Other".to_string(),
                bytes: 42,
                allocated_bytes: 42,
                files: 1,
            }],
            largest_files: vec![],
            largest_folders: vec![],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: Some("fnv1a64:test".to_string()),
            category_rules_source: Some("builtin".to_string()),
            scan_stats: ScanStats {
                elapsed_ms: 123,
                files_per_second: 8.0,
                folders_per_second: 4.0,
                logical_bytes_per_second: 42.0,
                allocated_bytes_per_second: 84.0,
            },
        };

        save_snapshot(&paths, &snapshot).expect("save");
        let loaded = load_snapshot(&paths).expect("load");
        assert_eq!(loaded.total_bytes, 42);
        assert_eq!(loaded.file_count, 1);
        assert_eq!(loaded.categories[0].name, "Other");
        assert_eq!(
            loaded.category_rules_fingerprint.as_deref(),
            Some("fnv1a64:test")
        );
        assert_eq!(loaded.scan_stats.elapsed_ms, 123);
    }

    #[test]
    fn workspace_name_validator_accepts_portable_names() {
        for name in ["default", "media-2026", "work.docs", "backup_1"] {
            validate_workspace_name(name).expect("valid workspace name");
        }
    }

    #[test]
    fn workspace_name_validator_rejects_unsafe_names_with_hint() {
        for name in ["", ".", "..", "media/photos", "bad name", "oops!"] {
            let err = validate_workspace_name(name).expect_err("invalid workspace name");
            let message = err.to_string();
            assert!(
                message.contains("invalid workspace name"),
                "unexpected error for {name:?}: {message}"
            );
        }
    }

    #[test]
    fn scan_temp_tree_finds_largest_files_and_folders() {
        let dir = temp_dir("scan_temp_tree");
        let docs = dir.join("docs");
        fs::create_dir(&docs).expect("mkdir");
        fs::write(docs.join("a.pdf"), vec![0; 2_048]).expect("write");
        fs::write(dir.join("video.mp4"), vec![0; 4_096]).expect("write");

        let category_rules =
            CategoryRules::from_config(CategoryConfig::default(), "builtin".to_string())
                .expect("category rules");
        let snapshot = scan_paths(
            std::slice::from_ref(&dir),
            &AppConfig::default(),
            &category_rules,
            10,
        )
        .expect("scan");

        assert_eq!(snapshot.file_count, 2);
        assert_eq!(snapshot.total_bytes, 6_144);
        assert!(snapshot.total_allocated_bytes > 0);
        assert_eq!(snapshot.largest_files[0].bytes, 4_096);
        assert!(snapshot.largest_files[0].allocated_bytes > 0);
        assert!(
            snapshot
                .categories
                .iter()
                .any(|category| category.name == "Videos"
                    && category.bytes == 4_096
                    && category.allocated_bytes > 0)
        );
        assert!(
            snapshot
                .largest_folders
                .iter()
                .any(|entry| entry.path == dir.display().to_string())
        );
        assert!(snapshot.category_rules_fingerprint.is_some());
        assert!(snapshot.scan_stats.elapsed_ms < u64::MAX);
    }

    #[test]
    fn top_files_rank_by_logical_size() {
        let mut top = TopList::new(2);
        top.push(sized_entry("smaller-allocated.bin", 100, 1_000));
        top.push(sized_entry("larger-logical.bin", 200, 200));
        top.push(sized_entry("middle.bin", 150, 150));

        let entries = top.into_sorted_vec();
        assert_eq!(entries[0].path, "larger-logical.bin");
        assert_eq!(entries[1].path, "middle.bin");
    }

    #[test]
    fn row_details_split_logical_and_on_disk_sizes() {
        let file = sized_entry("C:\\data\\large.iso", 200, 512);
        let file_row = row_for_entry(&file, EntryKind::File);
        assert_eq!(file_row.size_bytes, 200);
        assert_eq!(file_row.allocated_bytes, 512);
        assert_eq!(file_row.usage_bytes, 200);

        let mut folder = sized_entry("C:\\data", 200, 512);
        folder.files = Some(3);
        let folder_row = row_for_entry(&folder, EntryKind::Folder);
        assert_eq!(folder_row.size_bytes, 200);
        assert_eq!(folder_row.allocated_bytes, 512);
        assert_eq!(folder_row.usage_bytes, 512);
        assert_eq!(folder_row.detail, "3 files");
    }

    #[test]
    fn parses_numbered_detail_menu_choices() {
        assert_eq!(
            parse_detail_menu_choice("1 7", false, true).expect("parse"),
            Some((EntryKind::File, 7))
        );
        assert_eq!(
            parse_detail_menu_choice("2 3", false, false).expect("parse"),
            Some((EntryKind::Folder, 3))
        );
        assert!(is_detail_back_choice("2", false, true));
        assert!(is_detail_back_choice("3", false, false));
        assert!(is_quit_choice("q"));
    }

    #[cfg(windows)]
    #[test]
    fn detects_cloud_placeholder_attributes() {
        assert!(cloud_placeholder_attributes(0x0000_1000));
        assert!(cloud_placeholder_attributes(0x0004_0000));
        assert!(cloud_placeholder_attributes(0x0040_0000));
        assert!(!cloud_placeholder_attributes(0));
    }

    #[test]
    fn resolves_navigation_targets_from_cached_ranks() {
        let dir = temp_dir("navigation_targets");
        let nested = dir.join("nested");
        fs::create_dir(&nested).expect("mkdir");
        let file = nested.join("large.iso");
        fs::write(&file, vec![0; 1_024]).expect("write");
        let category_rules =
            CategoryRules::from_config(CategoryConfig::default(), "builtin".to_string())
                .expect("category rules");
        let snapshot = scan_paths(
            std::slice::from_ref(&dir),
            &AppConfig::default(),
            &category_rules,
            10,
        )
        .expect("scan");

        let file_navigation =
            resolve_navigation_path(&snapshot, NavigationTarget::File, 1, 10).expect("file nav");
        assert_eq!(file_navigation.path, file);
        assert!(file_navigation.select_file);

        let folder_navigation = resolve_navigation_path(&snapshot, NavigationTarget::Folder, 1, 10)
            .expect("folder nav");
        assert_eq!(folder_navigation.path, dir);
        assert!(!folder_navigation.select_file);

        let err = resolve_navigation_path(&snapshot, NavigationTarget::File, 2, 10)
            .expect_err("out of range");
        assert!(
            err.to_string()
                .contains("file number must be between 1 and 1")
        );
    }

    #[test]
    fn missing_cached_file_with_existing_parent_navigates_to_parent() {
        let dir = temp_dir("missing_file_parent_navigation");
        let parent = dir.join("folder with spaces");
        fs::create_dir(&parent).expect("mkdir");
        let missing_file = parent.join("gone file.tmp");
        let entry = SizedEntry {
            path: missing_file.display().to_string(),
            bytes: 948,
            allocated_bytes: 948,
            files: None,
            category: Some("Temporary / Cache".to_string()),
        };

        let navigation = navigation_for_entry(&entry, NavigationTarget::File).expect("navigation");

        assert_eq!(navigation.path, parent);
        assert!(!navigation.select_file);
        assert!(navigation.message.contains("cached file was not found"));
    }

    #[test]
    fn missing_cached_file_is_prune_eligible_even_when_parent_exists() {
        let dir = temp_dir("missing_file_prune_eligible");
        let parent = dir.join("cache");
        fs::create_dir(&parent).expect("mkdir");
        let entry = SizedEntry {
            path: parent.join("gone.bin").display().to_string(),
            bytes: 100,
            allocated_bytes: 200,
            files: None,
            category: Some("Other".to_string()),
        };

        assert!(entry_is_missing(&entry, NavigationTarget::File));
        assert!(navigation_for_entry(&entry, NavigationTarget::File).is_ok());
    }

    #[test]
    fn explorer_args_quote_spacey_paths_for_folder_and_select_modes() {
        let folder_navigation = NavigationPath {
            path: PathBuf::from(r"C:\Users\Example\Folder With Spaces"),
            select_file: false,
            message: String::new(),
        };
        assert_eq!(
            explorer_args_for_navigation(&folder_navigation),
            vec![r#"/e,"C:\Users\Example\Folder With Spaces""#.to_string()]
        );

        let file_navigation = NavigationPath {
            path: PathBuf::from(r"C:\Users\Example\Folder With Spaces\file.bin"),
            select_file: true,
            message: String::new(),
        };
        assert_eq!(
            explorer_args_for_navigation(&file_navigation),
            vec![r#"/select,"C:\Users\Example\Folder With Spaces\file.bin""#.to_string()]
        );
    }

    #[test]
    fn detects_empty_cached_folder_drilldown_lists() {
        let file_entry = sized_entry(r"C:\cache-test\child.bin", 100, 100);
        let folder_entry = SizedEntry {
            path: r"C:\cache-test\child".to_string(),
            bytes: 100,
            allocated_bytes: 100,
            files: Some(1),
            category: None,
        };

        assert!(cached_drilldown_entries_are_empty(&[], &[]));
        assert!(!cached_drilldown_entries_are_empty(
            std::slice::from_ref(&file_entry),
            &[]
        ));
        assert!(!cached_drilldown_entries_are_empty(
            &[],
            std::slice::from_ref(&folder_entry)
        ));
    }

    #[test]
    fn existing_cached_folder_builds_folder_open_navigation() {
        let dir = temp_dir("existing_folder_open_navigation");
        let entry = SizedEntry {
            path: dir.display().to_string(),
            bytes: 100,
            allocated_bytes: 100,
            files: Some(10),
            category: None,
        };

        let navigation =
            navigation_for_entry(&entry, NavigationTarget::Folder).expect("folder navigation");

        assert_eq!(navigation.path, dir);
        assert!(!navigation.select_file);
        assert!(navigation.message.contains("Opening Explorer at"));
    }

    #[test]
    fn missing_selected_folder_is_prune_eligible() {
        let dir = temp_dir("missing_folder_prune_eligible");
        let missing = dir.join("gone");
        let entry = SizedEntry {
            path: missing.display().to_string(),
            bytes: 100,
            allocated_bytes: 100,
            files: Some(4),
            category: None,
        };

        assert!(entry_is_missing(&entry, NavigationTarget::Folder));
        assert!(navigation_for_entry(&entry, NavigationTarget::Folder).is_err());
    }

    #[test]
    fn prunes_missing_cached_folder_without_rescan() {
        let mut snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time"),
            roots: vec![r"C:\cache-test".to_string()],
            total_bytes: 750,
            total_allocated_bytes: 1_200,
            total_capacity_bytes: Some(10_000),
            file_count: 3,
            folder_count: 5,
            categories: vec![CategoryTotal {
                name: "Other".to_string(),
                bytes: 300,
                allocated_bytes: 500,
                files: 2,
            }],
            largest_files: vec![
                sized_entry(r"C:\cache-test\project\gone\a.bin", 100, 200),
                sized_entry(r"C:\cache-test\project\gone\child\b.bin", 200, 300),
                sized_entry(r"C:\cache-test\keep\c.bin", 450, 700),
            ],
            largest_folders: vec![
                SizedEntry {
                    path: r"C:\cache-test".to_string(),
                    bytes: 750,
                    allocated_bytes: 1_200,
                    files: Some(3),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\project".to_string(),
                    bytes: 300,
                    allocated_bytes: 500,
                    files: Some(2),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\project\gone".to_string(),
                    bytes: 300,
                    allocated_bytes: 500,
                    files: Some(2),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\project\gone\child".to_string(),
                    bytes: 200,
                    allocated_bytes: 300,
                    files: Some(1),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\keep".to_string(),
                    bytes: 450,
                    allocated_bytes: 700,
                    files: Some(1),
                    category: None,
                },
            ],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: None,
            category_rules_source: None,
            scan_stats: ScanStats::default(),
        };
        let entry = snapshot.largest_folders[2].clone();

        let result = prune_cached_entry(&mut snapshot, NavigationTarget::Folder, &entry);

        assert_eq!(
            result,
            PruneResult {
                removed_files: 2,
                removed_folders: 2,
            }
        );
        assert_eq!(snapshot.total_bytes, 450);
        assert_eq!(snapshot.total_allocated_bytes, 700);
        assert_eq!(snapshot.file_count, 1);
        assert_eq!(snapshot.folder_count, 3);
        assert_eq!(snapshot.largest_files.len(), 1);
        assert_eq!(snapshot.largest_folders.len(), 3);
        assert_eq!(snapshot.largest_folders[0].path, r"C:\cache-test");
        assert_eq!(snapshot.largest_folders[0].bytes, 450);
        assert_eq!(snapshot.largest_folders[0].allocated_bytes, 700);
        assert_eq!(snapshot.largest_folders[0].files, Some(1));
        assert_eq!(snapshot.largest_folders[1].path, r"C:\cache-test\keep");
        assert_eq!(snapshot.largest_folders[1].bytes, 450);
        assert_eq!(snapshot.largest_folders[1].allocated_bytes, 700);
        assert_eq!(snapshot.largest_folders[1].files, Some(1));
        assert_eq!(snapshot.largest_folders[2].path, r"C:\cache-test\project");
        assert_eq!(snapshot.largest_folders[2].bytes, 0);
        assert_eq!(snapshot.largest_folders[2].allocated_bytes, 0);
        assert_eq!(snapshot.largest_folders[2].files, Some(0));
    }

    #[test]
    fn cleanup_journal_totals_include_human_sizes() {
        let dir = temp_dir("cleanup_journal_totals");
        let paths = test_cache_paths(&dir);
        let entry = sized_entry(r"C:\cache-test\gone.bin", 2_048, 4_096);
        let result = PruneResult {
            removed_files: 1,
            removed_folders: 0,
        };

        append_cleanup_journal(&paths, NavigationTarget::File, &entry, &result)
            .expect("append cleanup");
        let line = fs::read_to_string(&paths.cleanup_journal_path).expect("read cleanup journal");
        let entry_json: serde_json::Value =
            serde_json::from_str(line.trim()).expect("cleanup journal JSON");
        assert_eq!(entry_json["version"].as_u64(), Some(1));

        let totals = manual_cleanup_totals(&paths).expect("cleanup totals");
        assert_eq!(totals.events, 1);
        assert_eq!(totals.removed_files, 1);
        assert_eq!(totals.removed_folders, 0);
        assert_eq!(totals.bytes, 2_048);
        assert_eq!(totals.allocated_bytes, 4_096);
        assert_eq!(totals.human_size, "2.00 KB");
        assert_eq!(totals.human_on_disk, "4.00 KB");
    }

    #[test]
    fn restore_base_and_fast_forward_replay_cleanup_journal() {
        let dir = temp_dir("restore_fast_forward");
        let paths = test_cache_paths(&dir);
        let mut base = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time"),
            roots: vec![r"C:\cache-test".to_string()],
            total_bytes: 100,
            total_allocated_bytes: 200,
            total_capacity_bytes: Some(1_000),
            file_count: 1,
            folder_count: 1,
            categories: vec![CategoryTotal {
                name: "Other".to_string(),
                bytes: 100,
                allocated_bytes: 200,
                files: 1,
            }],
            largest_files: vec![sized_entry(r"C:\cache-test\gone.bin", 100, 200)],
            largest_folders: vec![SizedEntry {
                path: r"C:\cache-test".to_string(),
                bytes: 100,
                allocated_bytes: 200,
                files: Some(1),
                category: None,
            }],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: None,
            category_rules_source: None,
            scan_stats: ScanStats::default(),
        };
        base.scan_stats.elapsed_ms = 123;
        save_full_scan(&paths, &base).expect("save full scan");
        let entry = base.largest_files[0].clone();
        append_cleanup_journal(
            &paths,
            NavigationTarget::File,
            &entry,
            &PruneResult {
                removed_files: 1,
                removed_folders: 0,
            },
        )
        .expect("append cleanup");

        fast_forward_cache(&paths).expect("fast forward");
        let tracked = load_snapshot(&paths).expect("tracked snapshot");
        assert!(tracked.largest_files.is_empty());
        assert_eq!(tracked.total_allocated_bytes, 0);
        assert_eq!(cache_mode(&paths, &tracked), CacheMode::PresentTracked);

        restore_base_cache(&paths).expect("restore base");
        let restored = load_snapshot(&paths).expect("restored snapshot");
        assert_eq!(restored, base);
        assert_eq!(cache_mode(&paths, &restored), CacheMode::BaseScan);
    }

    #[test]
    fn fast_forward_replays_folder_cleanup_to_cached_ancestors() {
        let dir = temp_dir("fast_forward_folder_cleanup");
        let paths = test_cache_paths(&dir);
        let base = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time"),
            roots: vec![r"C:\cache-test".to_string()],
            total_bytes: 750,
            total_allocated_bytes: 1_200,
            total_capacity_bytes: Some(10_000),
            file_count: 3,
            folder_count: 4,
            categories: vec![CategoryTotal {
                name: "Other".to_string(),
                bytes: 750,
                allocated_bytes: 1_200,
                files: 3,
            }],
            largest_files: vec![
                sized_entry(r"C:\cache-test\project\gone\a.bin", 100, 200),
                sized_entry(r"C:\cache-test\project\gone\b.bin", 200, 300),
                sized_entry(r"C:\cache-test\keep\c.bin", 450, 700),
            ],
            largest_folders: vec![
                SizedEntry {
                    path: r"C:\cache-test".to_string(),
                    bytes: 750,
                    allocated_bytes: 1_200,
                    files: Some(3),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\project".to_string(),
                    bytes: 300,
                    allocated_bytes: 500,
                    files: Some(2),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\project\gone".to_string(),
                    bytes: 300,
                    allocated_bytes: 500,
                    files: Some(2),
                    category: None,
                },
                SizedEntry {
                    path: r"C:\cache-test\keep".to_string(),
                    bytes: 450,
                    allocated_bytes: 700,
                    files: Some(1),
                    category: None,
                },
            ],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: None,
            category_rules_source: None,
            scan_stats: ScanStats::default(),
        };
        save_full_scan(&paths, &base).expect("save full scan");
        let entry = base.largest_folders[2].clone();
        append_cleanup_journal(
            &paths,
            NavigationTarget::Folder,
            &entry,
            &PruneResult {
                removed_files: 2,
                removed_folders: 1,
            },
        )
        .expect("append cleanup");

        fast_forward_cache(&paths).expect("fast forward");
        let tracked = load_snapshot(&paths).expect("tracked snapshot");

        assert_eq!(tracked.total_bytes, 450);
        assert_eq!(tracked.total_allocated_bytes, 700);
        assert_eq!(tracked.file_count, 1);
        assert_eq!(tracked.folder_count, 3);
        assert_eq!(tracked.largest_files.len(), 1);
        assert_eq!(tracked.largest_folders[0].path, r"C:\cache-test");
        assert_eq!(tracked.largest_folders[0].bytes, 450);
        assert_eq!(tracked.largest_folders[0].allocated_bytes, 700);
        assert_eq!(tracked.largest_folders[0].files, Some(1));
        assert_eq!(tracked.largest_folders[1].path, r"C:\cache-test\keep");
        assert_eq!(tracked.largest_folders[2].path, r"C:\cache-test\project");
        assert_eq!(tracked.largest_folders[2].bytes, 0);
        assert_eq!(tracked.largest_folders[2].allocated_bytes, 0);
        assert_eq!(tracked.largest_folders[2].files, Some(0));
        assert_eq!(cache_mode(&paths, &tracked), CacheMode::PresentTracked);
    }

    #[test]
    fn cache_usage_savings_exclude_navigation() {
        let dir = temp_dir("cache_usage_savings");
        let paths = test_cache_paths(&dir);
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            scanned_at_unix: current_unix().expect("time"),
            roots: vec![r"C:\cache-test".to_string()],
            total_bytes: 1_000,
            total_allocated_bytes: 2_000,
            total_capacity_bytes: Some(10_000),
            file_count: 10,
            folder_count: 4,
            categories: vec![],
            largest_files: vec![],
            largest_folders: vec![],
            skipped: vec![],
            access_denied_count: 0,
            category_rules_fingerprint: None,
            category_rules_source: None,
            scan_stats: ScanStats {
                elapsed_ms: 500,
                files_per_second: 0.0,
                folders_per_second: 0.0,
                logical_bytes_per_second: 0.0,
                allocated_bytes_per_second: 0.0,
            },
        };
        save_full_scan(&paths, &snapshot).expect("save full scan");

        record_cache_usage(&paths, CacheUsageEventKind::Summary).expect("record summary");
        record_cache_usage(&paths, CacheUsageEventKind::CacheNavigation)
            .expect("record navigation");
        let first_line = fs::read_to_string(&paths.cache_usage_journal_path)
            .expect("read cache usage journal")
            .lines()
            .next()
            .expect("first cache usage line")
            .to_string();
        let entry_json: serde_json::Value =
            serde_json::from_str(&first_line).expect("cache usage journal JSON");
        assert_eq!(entry_json["version"].as_u64(), Some(1));

        let totals = cache_savings_totals(&paths).expect("savings totals");
        assert_eq!(totals.counted_readouts, 1);
        assert_eq!(totals.cache_navigation_count, 1);
        assert_eq!(totals.estimated_allocated_bytes_not_rewalked, 2_000);
        assert_eq!(totals.estimated_files_not_rechecked, 10);
        assert_eq!(totals.estimated_scan_time_saved_ms, 500);
    }

    #[test]
    fn unversioned_and_future_journal_entries_are_handled_explicitly() {
        let dir = temp_dir("journal_versions");
        let paths = test_cache_paths(&dir);
        fs::write(
            &paths.cleanup_journal_path,
            r#"{"timestamp":{"unix_seconds":1,"utc":"1970-01-01T00:00:01Z"},"target_type":"file","path":"old.bin","removed_files":1,"removed_folders":0,"bytes":10,"allocated_bytes":20,"human_size":"10 B","human_on_disk":"20 B","entry":{"path":"old.bin","bytes":10,"allocated_bytes":20,"files":null,"category":"Other"}}"#,
        )
        .expect("write legacy cleanup journal");
        assert_eq!(
            load_cleanup_journal(&paths).expect("legacy cleanup loads")[0].version,
            CLEANUP_JOURNAL_VERSION
        );

        fs::write(
            &paths.cache_usage_journal_path,
            r#"{"timestamp":{"unix_seconds":1,"utc":"1970-01-01T00:00:01Z"},"event":"summary","counts_as_readout":true,"basis_scanned_at_unix":1,"basis_total_bytes":10,"basis_total_allocated_bytes":20,"basis_file_count":1,"basis_folder_count":1,"basis_elapsed_ms":5}"#,
        )
        .expect("write legacy cache usage journal");
        assert_eq!(
            load_cache_usage_journal(&paths).expect("legacy usage loads")[0].version,
            CACHE_USAGE_JOURNAL_VERSION
        );

        fs::write(
            &paths.cleanup_journal_path,
            r#"{"version":2,"timestamp":{"unix_seconds":1,"utc":"1970-01-01T00:00:01Z"},"target_type":"file","path":"new.bin","removed_files":1,"removed_folders":0,"bytes":10,"allocated_bytes":20,"human_size":"10 B","human_on_disk":"20 B","entry":{"path":"new.bin","bytes":10,"allocated_bytes":20,"files":null,"category":"Other"}}"#,
        )
        .expect("write future cleanup journal");
        let err = load_cleanup_journal(&paths).expect_err("future cleanup should fail");
        assert!(
            err.to_string()
                .contains("unsupported cleanup journal version 2")
        );

        fs::write(
            &paths.cache_usage_journal_path,
            r#"{"version":2,"timestamp":{"unix_seconds":1,"utc":"1970-01-01T00:00:01Z"},"event":"summary","counts_as_readout":true,"basis_scanned_at_unix":1,"basis_total_bytes":10,"basis_total_allocated_bytes":20,"basis_file_count":1,"basis_folder_count":1,"basis_elapsed_ms":5}"#,
        )
        .expect("write future cache usage journal");
        let err = load_cache_usage_journal(&paths).expect_err("future usage should fail");
        assert!(
            err.to_string()
                .contains("unsupported cache usage journal version 2")
        );
    }

    fn sized_entry(path: &str, bytes: u64, allocated_bytes: u64) -> SizedEntry {
        SizedEntry {
            path: path.to_string(),
            bytes,
            allocated_bytes,
            files: None,
            category: Some("Other".to_string()),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("dscan-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn test_cache_paths(dir: &Path) -> CachePaths {
        CachePaths {
            app_dir: dir.to_path_buf(),
            base_dir: dir.to_path_buf(),
            workspaces_dir: dir.join("workspaces"),
            workspace_registry_path: dir.join("workspaces.json"),
            workspace_name: DEFAULT_WORKSPACE_NAME.to_string(),
            config_path: dir.join("config.json"),
            category_config_path: dir.join("categories.json"),
            snapshot_path: dir.join("snapshot.json"),
            base_snapshot_path: dir.join("base-snapshot.json"),
            cleanup_journal_path: dir.join("cleanup-journal.jsonl"),
            cache_usage_journal_path: dir.join("cache-usage-journal.jsonl"),
        }
    }
}
