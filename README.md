# dscan11

`dscan11` is a lightweight Windows CLI storage scanner. It scans when asked, stores a
workspace snapshot under `%LOCALAPPDATA%\dscan11\workspaces`, and serves later
summary/file/folder views from that cache without rescanning.

## Build

This workspace uses the Rust GNU Windows toolchain so it can build without Visual
Studio Build Tools:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
powershell -ExecutionPolicy Bypass -File scripts\cargo-gnu.ps1 test
powershell -ExecutionPolicy Bypass -File scripts\cargo-gnu.ps1 build --release
```

To update the local `dscan11` command on this machine, install the release build
into the single PATH location:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-local.ps1
```

This copies the release binary to `%LOCALAPPDATA%\Programs\dscan11\dscan11.exe`
and keeps the build output directory out of User PATH so Windows does not choose
between multiple installed copies. Open a new PowerShell window after install,
then check the command with `where.exe dscan11` and `dscan11 --version`.

## Usage

```powershell
dscan11 --help
dscan11 --version
dscan11 scan C:\Users\Example
dscan11 summary
dscan11 files
dscan11 folders
dscan11 --workspace ai-dev-audit --json --quiet audit C:\Users\Example --rules audit-rules.json
dscan11 open file 1
dscan11 open folder 1
dscan11 status
dscan11 workspace list
dscan11 workspace create media
dscan11 workspace use media
dscan11 --workspace media summary
dscan11 cache restore-base
dscan11 cache fast-forward
dscan11 cache cleanups
dscan11 config --stale-days 15
dscan11 config --init-categories
```

If no paths are passed to `scan`, `dscan11` scans existing Windows drive roots such
as `C:\`. Workspaces let you keep multiple independent scan caches: one
workspace is globally active by default, and `--workspace NAME` can target a
specific workspace for a single command. Workspace names may contain only
letters, numbers, dots, dashes, and underscores. Use `scan --top N` to choose how
many largest files and folders are stored in the cache. Use `--limit N` with
`summary`, `files`, or `folders` to choose how many cached rows are displayed.
Use `open file N` to open Explorer at the folder containing the Nth largest
cached file, or `open folder N` to open the Nth largest cached folder directly.
Cache views never trigger a rescan, though successful views are logged to the
usage journal for savings estimates.
Online-only OneDrive/cloud placeholders count as `0 B` on disk until they are
hydrated locally.

Audit mode is built for storage oversight agents. Use a dedicated workspace for
the audit scope, pass one root path plus `--rules RULES.json`, and prefer
`--json --quiet` for automation:

```powershell
dscan11 --workspace ai-dev-audit --json --quiet audit C:\Users\Example --rules audit-rules.json
```

Audit reports use an object-shaped JSON envelope with schema version, command,
workspace, roots, generated timestamp, alerts, largest folders, and any
available project growth deltas. The rules file can define
`known_top_level_folders`, shared `generated_path_names`, and `projects` with
`path`, `max_size_mb`, `max_git_size_mb`, `growth_alert_mb`, and
`watch_generated_outputs`. Audit mode reports only; it never deletes files or
updates cleanup journals.

Audit rule helpers let external agents validate or bootstrap policy files:

```powershell
dscan11 audit rules validate audit-rules.json
dscan11 audit rules init audit-rules.json
dscan11 audit rules from-inventory WORKSPACE_INVENTORY.md --out audit-rules.json
```

When you confirm removal of a missing cached file or folder, `dscan11` writes an
append-only cleanup journal and updates only the active cache snapshot. The last
full scan remains available as `base-snapshot.json`, so `dscan11 cache
restore-base` can return to the original scan and `dscan11 cache fast-forward`
can replay the tracked manual removals back to the present state. Use `dscan11
cache cleanups` to list the manually tracked removals so far.

Agents and scripts can mark already-deleted cached paths without Explorer or an
interactive prompt:

```powershell
dscan11 --workspace ai-dev-audit cache mark-removed folder --path "C:\Users\Example\project\.venv" --yes
dscan11 --workspace ai-dev-audit --json cache mark-removed file --path "C:\Users\Example\Downloads\old.iso" --yes
```

`--yes` confirms that the path is already gone from disk and allows dscan11 to
append the cleanup journal entry and update only the active cache snapshot. The
command fails if the path still exists or is not present in the active cached
top-N file/folder list.

Each workspace tracks one scan scope. If you try to scan different roots into a
workspace that already has a snapshot, interactive terminals will suggest
creating or switching to a workspace dedicated to that scan; scripted and JSON
runs fail with a clear next step. Existing single-cache installs are adopted into
workspace `default` automatically.

Local companion files include schema versions so future incompatible state is
recognized cleanly. Existing unversioned config, category, workspace registry,
and journal files are treated as legacy v1 and remain readable; newly created or
rewritten files include their version. If a file was written by a newer
unsupported dscan11, the command fails with a clear upgrade or refresh message.

Category rules are loaded from `%LOCALAPPDATA%\dscan11\categories.json` only for
`scan` and `status`. If the file is missing, built-in defaults are used. To
create an editable file with all defaults already filled in, run:

```powershell
dscan11 config --init-categories
```

The file shape is:

```json
{
  "categories": {
    "Videos": ["mp4", "mkv", "mov"],
    "Documents": ["pdf", "docx", "txt"]
  },
  "path_rules": {
    "AI Models": [".ollama/models", ".cache/huggingface/hub"],
    "Docker / Containers": [
      "AppData/Local/Docker/wsl",
      "ProgramData/docker/containers"
    ]
  }
}
```

Path rules are normalized case-insensitively and match either `\` or `/`.
They run before extension rules, so Docker-owned `.vhdx` files and extensionless
model blobs are categorized by storage root first. OneDrive is a lower-priority
fallback for remaining unmatched files.

After a scan, `status` reports whether category rules have changed since the
snapshot was written, plus initial scan performance rates. These rates describe
the scanner's average throughput for that run, not raw disk benchmark results.
Status also reports manual cleanup totals and estimated scan work avoided by
cache views. The savings estimate treats each counted cache readout as one
avoided full readout of the latest scanned scope; Explorer `open` navigation is
tracked separately and does not increase estimated scan savings.

Run `dscan11 --help` for the complete command guide, flags, examples, cache
locations, and exit codes.
