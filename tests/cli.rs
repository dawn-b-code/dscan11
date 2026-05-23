use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn cli_scan_and_cached_views_work_on_temp_tree() {
    let cache = temp_dir("cache");
    let tree = temp_dir("tree");
    std::fs::write(tree.join("big.iso"), vec![0; 8_192]).expect("write iso");
    std::fs::write(tree.join("notes.txt"), vec![0; 1_024]).expect("write notes");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("summary");
    assert_success(summary, "Disk Images / VMs");

    let mut files = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    files.env("LOCALAPPDATA", &cache).arg("files");
    assert_success(files, "big.iso");

    let mut folders = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    folders
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("folders");
    assert_success(folders, "dscan-tree");

    let mut status = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    status.env("LOCALAPPDATA", &cache).arg("status");
    let output = status.output().expect("run status");
    assert!(
        output.status.success(),
        "status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for header in [
        "Scan status",
        "Scan freshness",
        "scanned at:",
        "stale on:",
        "Cache",
        "Drive scope",
        "Inventory",
        "Manual cleanups",
        "Cache savings",
        "Category rules",
        "Performance",
    ] {
        assert!(
            stdout.contains(header),
            "status output did not contain header {header:?}:\n{stdout}"
        );
    }
}

#[test]
fn status_reports_cache_tracking_and_savings_without_counting_status() {
    let cache = temp_dir("tracking-cache");
    let tree = temp_dir("tracking-tree");
    std::fs::write(tree.join("big.iso"), vec![0; 8_192]).expect("write iso");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "Scan status");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary.env("LOCALAPPDATA", &cache).arg("summary");
    assert_success(summary, "Disk Images / VMs");

    let first_status = status_json(&cache);
    assert_eq!(first_status["cache_mode"].as_str(), Some("base_scan"));
    assert!(
        first_status["scanned_at_utc"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z')),
        "status JSON should include RFC3339 UTC scan date"
    );
    assert!(
        first_status["stale_at_utc"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z')),
        "status JSON should include computed RFC3339 UTC stale date"
    );
    assert_eq!(
        first_status["cache_savings"]["counted_readouts"].as_u64(),
        Some(1)
    );
    assert_eq!(
        first_status["cache_savings"]["cache_navigation_count"].as_u64(),
        Some(0)
    );
    assert_eq!(first_status["manual_cleanups"]["events"].as_u64(), Some(0));

    let second_status = status_json(&cache);
    assert_eq!(
        second_status["cache_savings"]["counted_readouts"].as_u64(),
        Some(1),
        "status should not count as avoided scan work"
    );
}

#[test]
fn forced_full_scan_resets_active_journals_for_new_base() {
    let cache = temp_dir("reset-cache");
    let tree = temp_dir("reset-tree");
    let file = tree.join("big.iso");
    std::fs::write(&file, vec![0; 8_192]).expect("write iso");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "Scan status");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary.env("LOCALAPPDATA", &cache).arg("summary");
    assert_success(summary, "Disk Images / VMs");
    assert_eq!(
        status_json(&cache)["cache_savings"]["counted_readouts"].as_u64(),
        Some(1)
    );

    std::fs::write(&file, vec![0; 16_384]).expect("rewrite iso");
    let mut rescan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    rescan
        .env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg("--force")
        .arg(&tree);
    assert_success(rescan, "Scan status");

    let status = status_json(&cache);
    assert_eq!(status["cache_mode"].as_str(), Some("base_scan"));
    assert_eq!(
        status["cache_savings"]["counted_readouts"].as_u64(),
        Some(0)
    );
    let dscan = cache.join("dscan11").join("workspaces").join("default");
    assert!(
        std::fs::read_dir(&dscan)
            .expect("read cache dir")
            .any(|entry| entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .contains("cache-usage-journal.jsonl.")),
        "full rescan should archive previous usage journal"
    );
}

#[test]
fn json_limit_applies_to_cached_views() {
    let cache = temp_dir("limit-cache");
    let tree = temp_dir("limit-tree");
    std::fs::write(tree.join("big.iso"), vec![0; 8_192]).expect("write iso");
    std::fs::write(tree.join("notes.txt"), vec![0; 1_024]).expect("write notes");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg("--top")
        .arg("5")
        .arg(&tree);
    assert_success(scan, "");

    let mut files = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    files
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("--limit")
        .arg("1")
        .arg("files");
    let output = files.output().expect("run files");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("files emits JSON");
    assert_eq!(rows.as_array().expect("array").len(), 1);
}

#[test]
fn fresh_scan_without_force_uses_cached_status_in_noninteractive_mode() {
    let cache = temp_dir("fresh-cache");
    let tree = temp_dir("fresh-tree");
    let file = tree.join("notes.txt");
    std::fs::write(&file, vec![0; 1_024]).expect("write notes");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "Scan status");

    std::fs::write(&file, vec![0; 2_048]).expect("rewrite notes");

    let mut second_scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    second_scan
        .env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg(&tree);
    let output = second_scan.output().expect("run second scan");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Scan status"),
        "fresh scan should show previous cached status:\n{stdout}"
    );
    assert_file_bytes(&cache, 1_024);

    let mut forced_scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    forced_scan
        .env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg("--force")
        .arg(&tree);
    assert_success(forced_scan, "Scan status");
    assert_file_bytes(&cache, 2_048);
}

#[test]
fn status_reports_changed_category_rules_but_summary_stays_snapshot_only() {
    let cache = temp_dir("category-cache");
    let tree = temp_dir("category-tree");
    std::fs::write(tree.join("movie.mkv"), vec![0; 1_024]).expect("write movie");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg("--force")
        .arg(&tree);
    assert_success(scan, "Scan status");

    let category_path = cache.join("dscan11").join("categories.json");
    std::fs::write(
        &category_path,
        r#"{"categories":{"Custom Video":["mkv"],"Documents":["txt"]}}"#,
    )
    .expect("write categories");

    let mut status = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    status.env("LOCALAPPDATA", &cache).arg("status");
    assert_success(status, "category rules: changed since scan");

    std::fs::write(&category_path, "{not valid json").expect("break categories");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("summary");
    assert_success(summary, "Videos");
}

#[test]
fn config_init_categories_creates_default_category_config() {
    let cache = temp_dir("init-categories-cache");

    let mut init = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    init.env("LOCALAPPDATA", &cache)
        .arg("config")
        .arg("--init-categories");
    assert_success(init, "Created category config from defaults");

    let category_path = cache.join("dscan11").join("categories.json");
    let contents = std::fs::read_to_string(&category_path).expect("read categories");
    let categories: serde_json::Value =
        serde_json::from_str(&contents).expect("categories are valid JSON");
    assert_eq!(
        categories["categories"]["Videos"]
            .as_array()
            .expect("Videos array")
            .iter()
            .any(|value| value == "mkv"),
        true
    );
    assert_eq!(
        categories["categories"]["Documents"]
            .as_array()
            .expect("Documents array")
            .iter()
            .any(|value| value == "pdf"),
        true
    );
    assert_eq!(
        categories["path_rules"]["AI Models"]
            .as_array()
            .expect("AI Models path rules")
            .iter()
            .any(|value| value == ".ollama/models"),
        true
    );
    assert_eq!(
        categories["path_rules"]["Docker / Containers"]
            .as_array()
            .expect("Docker path rules")
            .iter()
            .any(|value| value == "ProgramData/docker/containers"),
        true
    );
}

#[test]
fn scan_classifies_path_rule_storage_roots() {
    let cache = temp_dir("path-rules-cache");
    let tree = temp_dir("path-rules-tree");
    let ollama_blobs = tree.join(".ollama").join("models").join("blobs");
    let docker_data = tree.join(".docker").join("desktop").join("vm-data");
    std::fs::create_dir_all(&ollama_blobs).expect("create ollama dirs");
    std::fs::create_dir_all(&docker_data).expect("create docker dirs");
    std::fs::write(ollama_blobs.join("sha256-abc"), vec![0; 4_096]).expect("write model blob");
    std::fs::write(docker_data.join("ext4.vhdx"), vec![0; 8_192]).expect("write docker vhdx");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "Scan status");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary.env("LOCALAPPDATA", &cache).arg("summary");
    assert_success(summary, "AI Models");

    let mut summary = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    summary.env("LOCALAPPDATA", &cache).arg("summary");
    assert_success(summary, "Docker / Containers");
}

#[test]
fn config_init_categories_reports_json_result() {
    let cache = temp_dir("init-categories-json-cache");

    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("config")
        .arg("--init-categories")
        .output()
        .expect("run config init categories");

    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bootstrap emits JSON");
    assert_eq!(result["created"].as_bool(), Some(true));
    assert_eq!(result["source"].as_str(), Some("defaults"));
    assert!(
        result["category_config_path"]
            .as_str()
            .expect("category path")
            .ends_with("categories.json")
    );
}

#[test]
fn config_init_categories_does_not_overwrite_existing_file() {
    let cache = temp_dir("init-categories-existing-cache");
    let dscan = cache.join("dscan11");
    std::fs::create_dir_all(&dscan).expect("create dscan dir");
    let category_path = dscan.join("categories.json");
    let custom = r#"{"categories":{"Custom":["abc"]}}"#;
    std::fs::write(&category_path, custom).expect("write custom categories");

    let mut init = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    init.env("LOCALAPPDATA", &cache)
        .arg("config")
        .arg("--init-categories");
    assert_success(init, "Category config already exists");

    let contents = std::fs::read_to_string(&category_path).expect("read categories");
    assert_eq!(contents, custom);
}

#[test]
fn scan_missing_root_fails_without_cache_snapshot() {
    let cache = temp_dir("missing-cache");
    let missing = temp_dir("missing-root").join("does-not-exist");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&missing);
    let output = scan.output().expect("run scan");

    assert!(!output.status.success(), "missing root should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scan root does not exist or is not a directory"),
        "unexpected stderr:\n{stderr}"
    );
    assert!(
        !cache
            .join("dscan11")
            .join("workspaces")
            .join("default")
            .join("snapshot.json")
            .exists(),
        "failed scan should not save a snapshot"
    );
}

#[test]
fn legacy_singleton_cache_is_adopted_as_default_workspace() {
    let cache = temp_dir("legacy-cache");
    let tree = temp_dir("legacy-tree");
    std::fs::write(tree.join("legacy.iso"), vec![0; 4_096]).expect("write legacy");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&tree);
    assert_success(scan, "Scan status");

    let app = cache.join("dscan11");
    let default = app.join("workspaces").join("default");
    for name in [
        "snapshot.json",
        "base-snapshot.json",
        "cleanup-journal.jsonl",
        "cache-usage-journal.jsonl",
    ] {
        let from = default.join(name);
        if from.exists() {
            std::fs::rename(&from, app.join(name)).expect("restore legacy file");
        }
    }
    std::fs::remove_file(app.join("workspaces.json")).expect("remove registry");
    std::fs::remove_dir_all(app.join("workspaces")).expect("remove workspaces dir");

    let status = status_json(&cache);
    assert_eq!(status["workspace"].as_str(), Some("default"));
    assert!(
        app.join("workspaces")
            .join("default")
            .join("snapshot.json")
            .exists(),
        "legacy snapshot should move into default workspace"
    );
}

#[test]
fn workspaces_keep_independent_scan_caches() {
    let cache = temp_dir("multi-cache");
    let media = temp_dir("media-tree");
    let docs = temp_dir("docs-tree");
    std::fs::write(media.join("movie.mkv"), vec![0; 6_144]).expect("write movie");
    std::fs::write(docs.join("paper.pdf"), vec![0; 2_048]).expect("write paper");

    let mut create_media = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    create_media
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("create")
        .arg("media");
    assert_success(create_media, "Created workspace");

    let mut scan_media = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan_media
        .env("LOCALAPPDATA", &cache)
        .arg("--workspace")
        .arg("media")
        .arg("scan")
        .arg(&media);
    assert_success(scan_media, "Scan status");

    let mut create_docs = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    create_docs
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("create")
        .arg("docs");
    assert_success(create_docs, "Created workspace");

    let mut scan_docs = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan_docs
        .env("LOCALAPPDATA", &cache)
        .arg("--workspace")
        .arg("docs")
        .arg("scan")
        .arg(&docs);
    assert_success(scan_docs, "Scan status");

    let mut media_files = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    media_files
        .env("LOCALAPPDATA", &cache)
        .arg("--workspace")
        .arg("media")
        .arg("files");
    assert_success(media_files, "movie.mkv");

    let mut docs_files = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    docs_files
        .env("LOCALAPPDATA", &cache)
        .arg("--workspace")
        .arg("docs")
        .arg("files");
    assert_success(docs_files, "paper.pdf");
}

#[test]
fn workspace_override_does_not_change_active_workspace() {
    let cache = temp_dir("override-cache");
    let tree = temp_dir("override-tree");
    std::fs::write(tree.join("movie.mkv"), vec![0; 1_024]).expect("write movie");

    let mut create = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    create
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("create")
        .arg("media");
    assert_success(create, "Created workspace");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache)
        .arg("--workspace")
        .arg("media")
        .arg("scan")
        .arg(&tree);
    assert_success(scan, "Scan status");

    let current = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .env("LOCALAPPDATA", &cache)
        .arg("--json")
        .arg("workspace")
        .arg("current")
        .output()
        .expect("run current");
    assert!(current.status.success(), "current workspace failed");
    let json: serde_json::Value =
        serde_json::from_slice(&current.stdout).expect("current emits JSON");
    assert_eq!(json["name"].as_str(), Some("default"));
}

#[test]
fn invalid_workspace_names_are_rejected_with_hint() {
    for name in ["", ".", "..", "media/photos", "bad name", "oops!"] {
        let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
            .arg("workspace")
            .arg("create")
            .arg(name)
            .output()
            .expect("run workspace create");
        assert!(!output.status.success(), "name {name:?} should fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("invalid workspace name"),
            "unexpected stderr for {name:?}:\n{stderr}"
        );
    }
}

#[test]
fn workspace_lifecycle_commands_work() {
    let cache = temp_dir("lifecycle-cache");

    let mut create = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    create
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("create")
        .arg("draft");
    assert_success(create, "Created workspace");

    let mut rename = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    rename
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("rename")
        .arg("draft")
        .arg("archive");
    assert_success(rename, "Renamed workspace");

    let mut use_workspace = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    use_workspace
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("use")
        .arg("archive");
    assert_success(use_workspace, "Using workspace");

    let mut switch_default = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    switch_default
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("use")
        .arg("default");
    assert_success(switch_default, "Using workspace");

    let mut delete = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    delete
        .env("LOCALAPPDATA", &cache)
        .arg("workspace")
        .arg("delete")
        .arg("--force")
        .arg("archive");
    assert_success(delete, "Deleted workspace");
}

#[test]
fn scan_root_mismatch_fails_in_noninteractive_mode() {
    let cache = temp_dir("mismatch-cache");
    let first = temp_dir("mismatch-first");
    let second = temp_dir("mismatch-second");
    std::fs::write(first.join("first.txt"), vec![0; 1_024]).expect("write first");
    std::fs::write(second.join("second.txt"), vec![0; 1_024]).expect("write second");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_dscan11"));
    scan.env("LOCALAPPDATA", &cache).arg("scan").arg(&first);
    assert_success(scan, "Scan status");

    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .env("LOCALAPPDATA", &cache)
        .arg("scan")
        .arg(&second)
        .output()
        .expect("run mismatch scan");
    assert!(!output.status.success(), "mismatch scan should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scan roots differ from workspace `default`"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn largest_command_is_removed() {
    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .arg("largest")
        .output()
        .expect("run largest");

    assert!(!output.status.success(), "largest should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown command `largest`"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn help_documents_commands_flags_and_examples() {
    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .arg("--help")
        .output()
        .expect("run help");
    assert!(
        output.status.success(),
        "help failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "Usage:",
        "Global options:",
        "--workspace NAME",
        "Commands:",
        "scan [--force] [--top N] [paths...]",
        "summary",
        "files",
        "folders",
        "open file N",
        "open folder N",
        "browse",
        "cache restore-base",
        "cache fast-forward",
        "workspace list",
        "workspace create NAME",
        "workspace delete [--force] NAME",
        "status",
        "config [--stale-days DAYS] [--init-categories]",
        "--top controls how much scan data is stored.",
        "--limit controls how much cached data is displayed.",
        "JSON output respects --limit",
        "Exit codes:",
    ] {
        assert!(
            stdout.contains(needle),
            "help output did not contain {needle:?}:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("  largest\n"),
        "help should not document removed largest command:\n{stdout}"
    );
    assert!(
        !stdout.contains(&personal_path_marker()),
        "help should not contain personal local paths:\n{stdout}"
    );
}

#[test]
fn readme_uses_generic_public_paths() {
    let readme = std::fs::read_to_string("README.md").expect("read README");
    assert!(
        !readme.contains(&personal_path_marker()),
        "README should not contain personal local paths"
    );
    assert!(readme.contains(r"C:\Users\Example"));
}

fn personal_path_marker() -> String {
    [r"C:\Users", "Dawn"].join(r"\")
}

fn assert_success(mut command: Command, needle: &str) {
    let output = command.output().expect("run command");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !needle.is_empty() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(needle),
            "stdout did not contain {needle:?}:\n{stdout}"
        );
    }
}

fn assert_file_bytes(cache: &PathBuf, expected: u64) {
    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .env("LOCALAPPDATA", cache)
        .arg("--json")
        .arg("files")
        .output()
        .expect("run files");
    assert!(
        output.status.success(),
        "files failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("files emits JSON");
    assert_eq!(rows[0]["bytes"].as_u64(), Some(expected));
}

fn status_json(cache: &PathBuf) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_dscan11"))
        .env("LOCALAPPDATA", cache)
        .arg("--json")
        .arg("status")
        .output()
        .expect("run status");
    assert!(
        output.status.success(),
        "status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status emits JSON")
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("dscan-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}
