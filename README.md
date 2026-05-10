# dscan11

`dscan11` is a lightweight Windows CLI storage scanner. It scans when asked, stores a
snapshot under `%LOCALAPPDATA%\dscan11`, and serves later summary/file/folder
views from that cache without rescanning.

## Build

This workspace uses the Rust GNU Windows toolchain so it can build without Visual
Studio Build Tools:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
powershell -ExecutionPolicy Bypass -File scripts\cargo-gnu.ps1 test
powershell -ExecutionPolicy Bypass -File scripts\cargo-gnu.ps1 build --release
```

## Usage

```powershell
dscan11 --help
dscan11 scan C:\Users\Example
dscan11 summary
dscan11 files
dscan11 folders
dscan11 open file 1
dscan11 open folder 1
dscan11 status
dscan11 cache restore-base
dscan11 cache fast-forward
dscan11 cache cleanups
dscan11 config --stale-days 15
dscan11 config --init-categories
```

If no paths are passed to `scan`, `dscan11` scans existing Windows drive roots such
as `C:\`. Use `scan --top N` to choose how many largest files and folders are
stored in the cache. Use `--limit N` with `summary`, `files`, or `folders` to
choose how many cached rows are displayed. Use `open file N` to open Explorer at
the folder containing the Nth largest cached file, or `open folder N` to open the
Nth largest cached folder directly. Cache views never trigger a rescan, though
successful views are logged to the usage journal for savings estimates.
Online-only OneDrive/cloud placeholders count as `0 B` on disk until they are
hydrated locally.

When you confirm removal of a missing cached file or folder, `dscan11` writes an
append-only cleanup journal and updates only the active cache snapshot. The last
full scan remains available as `base-snapshot.json`, so `dscan11 cache
restore-base` can return to the original scan and `dscan11 cache fast-forward`
can replay the tracked manual removals back to the present state. Use `dscan11
cache cleanups` to list the manually tracked removals so far.

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
  }
}
```

After a scan, `status` reports whether category rules have changed since the
snapshot was written, plus effective average scan rates. These rates describe the
scanner's average throughput for that run, not raw disk benchmark results.
Status also reports manual cleanup totals and estimated scan work avoided by
cache views. The savings estimate treats each counted cache readout as one
avoided full readout of the latest scanned scope; Explorer `open` navigation is
tracked separately and does not increase estimated scan savings.

Run `dscan11 --help` for the complete command guide, flags, examples, cache
locations, and exit codes.
