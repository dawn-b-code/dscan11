use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use dscan11::{
    CacheUsageEventKind, CategoryConfigBootstrap, CliError, NavigationTarget, OutputMode,
    StaleInfo, cache_paths, discover_default_roots, fast_forward_cache, init_category_config,
    load_category_rules, load_or_default_config, load_snapshot, open_cached_path, print_browse,
    print_cleanup_journal, print_config, print_files, print_folders, print_json, print_status,
    print_summary, record_cache_usage, restore_base_cache, roots_match, save_config,
    save_full_scan, scan_paths,
};

struct Cli {
    json: bool,
    limit: usize,
    command: Commands,
}

enum Commands {
    Scan {
        paths: Vec<PathBuf>,
        top: Option<usize>,
        force: bool,
    },
    Summary,
    Files,
    Folders,
    Open {
        target: NavigationTarget,
        index: usize,
    },
    Browse,
    Status,
    Cache {
        action: CacheAction,
    },
    Config {
        stale_days: Option<u64>,
        init_categories: bool,
    },
}

enum CacheAction {
    RestoreBase,
    FastForward,
    Cleanups,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(err.exit_code());
    }
}

fn run() -> Result<(), CliError> {
    let cli = parse_cli(std::env::args().skip(1).collect())?;
    let paths = cache_paths()?;
    let mut config = load_or_default_config(&paths)?;
    let output = OutputMode::from_json(cli.json);

    match cli.command {
        Commands::Scan {
            paths: requested_paths,
            top,
            force,
        } => {
            let roots = if requested_paths.is_empty() {
                discover_default_roots()?
            } else {
                requested_paths
            };
            let top_limit = top.unwrap_or(config.top_limit).max(1);
            let category_rules = load_category_rules(&paths)?;
            if !force {
                if let Ok(snapshot) = load_snapshot(&paths) {
                    let stale = snapshot.stale_info(config.stale_days);
                    if !stale.is_stale && roots_match(&roots, &snapshot.roots) {
                        eprintln!(
                            "Cached scan is still fresh ({} days old; stale after {} days).",
                            stale.age_days, stale.stale_after_days
                        );
                        if output.is_json() || !io::stdin().is_terminal() {
                            print_status(
                                &snapshot,
                                &paths,
                                &config,
                                Some(&category_rules),
                                output,
                            )?;
                            record_cache_usage(&paths, CacheUsageEventKind::ScanAutoSkip)?;
                            return Ok(());
                        }
                        if !confirm_rescan()? {
                            print_status(
                                &snapshot,
                                &paths,
                                &config,
                                Some(&category_rules),
                                output,
                            )?;
                            record_cache_usage(&paths, CacheUsageEventKind::ScanAutoSkip)?;
                            return Ok(());
                        }
                    }
                }
            }
            let heartbeat = ScanHeartbeat::start(output);
            let snapshot = scan_paths(&roots, &config, &category_rules, top_limit)?;
            drop(heartbeat);
            save_full_scan(&paths, &snapshot)?;
            print_status(&snapshot, &paths, &config, Some(&category_rules), output)?;
        }
        Commands::Summary => {
            let snapshot = load_snapshot(&paths)?;
            warn_if_stale(&snapshot.stale_info(config.stale_days), output);
            print_summary(&snapshot, output, cli.limit)?;
            record_cache_usage(&paths, CacheUsageEventKind::Summary)?;
        }
        Commands::Files => {
            let snapshot = load_snapshot(&paths)?;
            warn_if_stale(&snapshot.stale_info(config.stale_days), output);
            print_files(&snapshot, output, cli.limit)?;
            record_cache_usage(&paths, CacheUsageEventKind::Files)?;
        }
        Commands::Folders => {
            let snapshot = load_snapshot(&paths)?;
            warn_if_stale(&snapshot.stale_info(config.stale_days), output);
            print_folders(&snapshot, output, cli.limit)?;
            record_cache_usage(&paths, CacheUsageEventKind::Folders)?;
        }
        Commands::Open { target, index } => {
            if output.is_json() {
                return Err(CliError::Message(
                    "open launches a file browser and does not support --json".to_string(),
                ));
            }
            let mut snapshot = load_snapshot(&paths)?;
            warn_if_stale(&snapshot.stale_info(config.stale_days), output);
            open_cached_path(&paths, &mut snapshot, target, index, cli.limit)?;
            record_cache_usage(&paths, CacheUsageEventKind::CacheNavigation)?;
        }
        Commands::Browse => {
            let mut snapshot = load_snapshot(&paths)?;
            warn_if_stale(&snapshot.stale_info(config.stale_days), output);
            print_browse(&paths, &mut snapshot, output, cli.limit)?;
            record_cache_usage(&paths, CacheUsageEventKind::Browse)?;
        }
        Commands::Status => {
            let snapshot = load_snapshot(&paths)?;
            let category_rules = load_category_rules(&paths)?;
            print_status(&snapshot, &paths, &config, Some(&category_rules), output)?;
        }
        Commands::Cache { action } => match action {
            CacheAction::RestoreBase => {
                if output.is_json() {
                    return Err(CliError::Message(
                        "cache restore-base does not support --json".to_string(),
                    ));
                }
                restore_base_cache(&paths)?;
                println!("Restored active cache from base scan.");
            }
            CacheAction::FastForward => {
                if output.is_json() {
                    return Err(CliError::Message(
                        "cache fast-forward does not support --json".to_string(),
                    ));
                }
                fast_forward_cache(&paths)?;
                println!("Fast-forwarded active cache by replaying manual cleanup journal.");
            }
            CacheAction::Cleanups => {
                print_cleanup_journal(&paths, output)?;
            }
        },
        Commands::Config {
            stale_days,
            init_categories,
        } => {
            if let Some(days) = stale_days {
                config.stale_days = days;
                save_config(&paths, &config)?;
            }
            if init_categories {
                let result = init_category_config(&paths)?;
                print_category_config_bootstrap(&result, output)?;
                return Ok(());
            }
            print_config(&config, &paths, output)?;
        }
    }

    Ok(())
}

fn print_category_config_bootstrap(
    result: &CategoryConfigBootstrap,
    output: OutputMode,
) -> Result<(), CliError> {
    if output.is_json() {
        print_json(result)
    } else if result.created {
        println!(
            "Created category config from defaults: {}",
            result.category_config_path
        );
        Ok(())
    } else {
        println!(
            "Category config already exists: {}",
            result.category_config_path
        );
        println!("Delete it first or edit it directly.");
        Ok(())
    }
}

struct ScanHeartbeat {
    done: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl ScanHeartbeat {
    fn start(output: OutputMode) -> Option<Self> {
        if output.is_json() {
            return None;
        }

        eprint!("Scanning");
        let _ = io::stderr().flush();

        let (done, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            const PATTERN: &[u8] = b"--+--|";
            let mut index = 0usize;
            loop {
                match receiver.recv_timeout(Duration::from_secs(10)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        eprint!("{}", PATTERN[index % PATTERN.len()] as char);
                        let _ = io::stderr().flush();
                        index += 1;
                    }
                }
            }
        });

        Some(Self {
            done,
            handle: Some(handle),
        })
    }
}

impl Drop for ScanHeartbeat {
    fn drop(&mut self) {
        let _ = self.done.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        eprintln!();
    }
}

fn parse_cli(args: Vec<String>) -> Result<Cli, CliError> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        std::process::exit(0);
    }

    let mut json = false;
    let mut limit = 40usize;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--limit" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::Message("--limit requires a positive integer".to_string())
                })?;
                limit = value.parse().map_err(|_| {
                    CliError::Message("--limit must be a positive integer".to_string())
                })?;
                index += 2;
            }
            arg => {
                positional.push(arg.to_string());
                index += 1;
            }
        }
    }

    let Some(command) = positional.first().map(String::as_str) else {
        return Err(CliError::Message(
            "missing command; run `dscan11 --help`".to_string(),
        ));
    };
    let rest = &positional[1..];

    let command = match command {
        "scan" => {
            let mut paths = Vec::new();
            let mut top = None;
            let mut force = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--force" => {
                        force = true;
                        index += 1;
                    }
                    "--top" => {
                        let value = rest.get(index + 1).ok_or_else(|| {
                            CliError::Message("--top requires a positive integer".to_string())
                        })?;
                        top = Some(value.parse().map_err(|_| {
                            CliError::Message("--top must be a positive integer".to_string())
                        })?);
                        index += 2;
                    }
                    path => {
                        paths.push(PathBuf::from(path));
                        index += 1;
                    }
                }
            }
            Commands::Scan { paths, top, force }
        }
        "summary" => Commands::Summary,
        "files" => Commands::Files,
        "folders" => Commands::Folders,
        "open" => {
            if rest.len() != 2 {
                return Err(CliError::Message(
                    "open requires `file N` or `folder N`; run `dscan11 --help`".to_string(),
                ));
            }
            let target = match rest[0].as_str() {
                "file" | "files" => NavigationTarget::File,
                "folder" | "folders" => NavigationTarget::Folder,
                other => {
                    return Err(CliError::Message(format!(
                        "unknown open target `{other}`; use `file` or `folder`"
                    )));
                }
            };
            let index = rest[1].parse().map_err(|_| {
                CliError::Message("open number must be a positive integer".to_string())
            })?;
            Commands::Open { target, index }
        }
        "browse" => Commands::Browse,
        "status" => Commands::Status,
        "cache" => {
            if rest.len() != 1 {
                return Err(CliError::Message(
                    "cache requires `restore-base`, `fast-forward`, or `cleanups`; run `dscan11 --help`"
                        .to_string(),
                ));
            }
            let action = match rest[0].as_str() {
                "restore-base" => CacheAction::RestoreBase,
                "fast-forward" => CacheAction::FastForward,
                "cleanups" | "cleanup" | "removals" => CacheAction::Cleanups,
                other => {
                    return Err(CliError::Message(format!(
                        "unknown cache action `{other}`; use `restore-base`, `fast-forward`, or `cleanups`"
                    )));
                }
            };
            Commands::Cache { action }
        }
        "config" => {
            let mut stale_days = None;
            let mut init_categories = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--init-categories" => {
                        init_categories = true;
                        index += 1;
                    }
                    "--stale-days" => {
                        let value = rest.get(index + 1).ok_or_else(|| {
                            CliError::Message("--stale-days requires a number".to_string())
                        })?;
                        stale_days = Some(value.parse().map_err(|_| {
                            CliError::Message("--stale-days must be a number".to_string())
                        })?);
                        index += 2;
                    }
                    other => {
                        return Err(CliError::Message(format!(
                            "unknown config argument `{other}`"
                        )));
                    }
                }
            }
            Commands::Config {
                stale_days,
                init_categories,
            }
        }
        other => {
            return Err(CliError::Message(format!(
                "unknown command `{other}`; run `dscan11 --help`"
            )));
        }
    };

    Ok(Cli {
        json,
        limit: limit.max(1),
        command,
    })
}

fn print_help() {
    print!(
        r#"dscan11 - cached Windows drive scanner

Scans one or more directories, stores a snapshot under %LOCALAPPDATA%\dscan11,
and serves later summary, file, folder, navigation, status, and config
views from that cache without rescanning.

Usage:
  dscan11 [GLOBAL OPTIONS] <COMMAND> [COMMAND OPTIONS]
  dscan11 --help
  dscan11 -h

Global options:
  --json
      Print machine-readable JSON for commands that display data.

  --limit N
      Limit rows displayed by summary, files, and folders.
      Default: 40

  -h, --help
      Show this help page.

Commands:
  scan [--force] [--top N] [paths...]
      Scan paths and save a fresh cache snapshot.

      If no paths are provided, dscan11 scans all discovered Windows drive roots
      such as C:\. Explicit scan roots must exist and must be directories.

      --top N
          Choose how many largest files and folders are stored in the cache.
          Default: config top_limit, initially 1000.

      --force
          Rescan even when the cached scan is still fresh for the same roots.

      Examples:
          dscan11 scan C:\Users\Example
          dscan11 scan --force C:\Users\Example
          dscan11 scan --top 500 C:\Users\Example D:\Media
          dscan11 --json scan --top 100 C:\Users\Example\Downloads

  summary
      Show cached storage totals grouped by category.

      Examples:
          dscan11 summary
          dscan11 --limit 10 summary
          dscan11 --json --limit 10 summary

  files
      Show the largest cached files.

      Examples:
          dscan11 files
          dscan11 --limit 25 files
          dscan11 --json --limit 25 files

  folders
      Show the largest cached folders.

      Examples:
          dscan11 folders
          dscan11 --limit 25 folders
          dscan11 --json --limit 25 folders

  open file N
      Open Explorer at the folder containing the Nth largest cached file. If the
      file still exists, Explorer selects it.

      Examples:
          dscan11 files
          dscan11 open file 1
          dscan11 --limit 25 open file 12

  open folder N
      Open Explorer directly at the Nth largest cached folder.

      Examples:
          dscan11 folders
          dscan11 open folder 1
          dscan11 --limit 25 open folder 12

  browse
      Interactively browse cached top-N files, folders, and categories.
      This uses retained cache data only, not a complete file inventory. File
      and folder browse views can show full details and open selected items.

      Examples:
          dscan11 browse
          dscan11 --limit 25 browse

  status
      Show cache status, scan age, roots, totals, skipped paths, and access
      denied counts. Also shows tracked manual cleanup totals, active cache mode,
      and estimated scan work avoided by cache views.

      Examples:
          dscan11 status
          dscan11 --json status

  cache restore-base
      Restore the active cache snapshot to the exact latest full scan.

      Examples:
          dscan11 cache restore-base

  cache fast-forward
      Replay the manual cleanup journal onto the latest full scan and save that
      tracked present state as the active cache snapshot.

      Examples:
          dscan11 cache fast-forward

  cache cleanups
      List manually tracked removals from the cleanup journal.

      Examples:
          dscan11 cache cleanups
          dscan11 --json cache cleanups

  config [--stale-days DAYS] [--init-categories]
      Show the current config, update the stale-scan warning threshold, or
      bootstrap an editable category config from built-in defaults.

      --stale-days DAYS
          Set how many days old a cached scan can be before views warn that it
          is stale. Use 0 to disable stale warnings.

      --init-categories
          Create %LOCALAPPDATA%\dscan11\categories.json from built-in defaults.
          Existing category configs are not overwritten.

      Examples:
          dscan11 config
          dscan11 config --stale-days 15
          dscan11 config --init-categories
          dscan11 --json config
          dscan11 --json config --init-categories

Cache and config:
  Cache directory:
      %LOCALAPPDATA%\dscan11

  Snapshot:
      %LOCALAPPDATA%\dscan11\snapshot.json

  Base snapshot:
      %LOCALAPPDATA%\dscan11\base-snapshot.json

  Journals:
      %LOCALAPPDATA%\dscan11\cleanup-journal.jsonl
      %LOCALAPPDATA%\dscan11\cache-usage-journal.jsonl

  Config:
      %LOCALAPPDATA%\dscan11\config.json

  Category config:
      %LOCALAPPDATA%\dscan11\categories.json

  Defaults:
      stale_days: 15
      top_limit: 1000

Important behavior:
  - Cache views never trigger a rescan; successful views are logged for savings estimates.
  - Cache savings are estimates based on the latest full scan scope.
  - open and browse navigation use cached file and folder ranks.
  - Fresh cached scans ask before rescanning unless --force is used.
  - --top controls how much scan data is stored.
  - --limit controls how much cached data is displayed.
  - Global options must appear before the command.
  - JSON output respects --limit for summary, files, and folders.

Exit codes:
  0  Success
  1  I/O or JSON error
  2  Invalid command, arguments, or scan root
"#
    );
}

fn warn_if_stale(info: &StaleInfo, output: OutputMode) {
    if output.is_json() || !info.is_stale {
        return;
    }

    eprintln!(
        "Warning: cached scan is {} days old; stale threshold is {} days. Run `dscan11 scan` to refresh.",
        info.age_days, info.stale_after_days
    );
}

fn confirm_rescan() -> Result<bool, CliError> {
    confirm_rescan_keypress()
}

#[cfg(windows)]
fn confirm_rescan_keypress() -> Result<bool, CliError> {
    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    const TIMEOUT_SECONDS: u64 = 5;
    const PULSE: &[u8] = b"---->";

    eprint!("Proceed with rescan? [y/N] auto-skip in 5s [");
    let _ = io::stderr().flush();

    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if stdin.is_null() || stdin == INVALID_HANDLE_VALUE {
        return confirm_rescan_line();
    }

    let started = std::time::Instant::now();
    let mut invalid_key = false;
    let mut printed = 0usize;

    loop {
        let elapsed = started.elapsed();
        while printed < elapsed.as_secs().min(TIMEOUT_SECONDS) as usize {
            eprint!("{}", PULSE[printed % PULSE.len()] as char);
            let _ = io::stderr().flush();
            printed += 1;
        }

        if elapsed >= Duration::from_secs(TIMEOUT_SECONDS) {
            while printed < TIMEOUT_SECONDS as usize {
                eprint!("{}", PULSE[printed % PULSE.len()] as char);
                printed += 1;
            }
            eprintln!("] auto-skip");
            return Ok(false);
        }

        let next_pulse = Duration::from_secs((printed as u64).saturating_add(1));
        let deadline = Duration::from_secs(TIMEOUT_SECONDS);
        let wait_for = next_pulse.min(deadline).saturating_sub(elapsed);
        let wait_ms = wait_for.as_millis().min(u128::from(u32::MAX)) as u32;

        let wait = unsafe { WaitForSingleObject(stdin, wait_ms.max(1)) };
        match wait {
            WAIT_TIMEOUT => {}
            WAIT_OBJECT_0 => {
                let mut record = INPUT_RECORD {
                    EventType: 0,
                    Event: unsafe { std::mem::zeroed() },
                };
                let mut read = 0u32;
                let ok = unsafe { ReadConsoleInputW(stdin, &mut record, 1, &mut read) } != 0;
                if !ok || read != 1 {
                    continue;
                }
                if record.EventType != KEY_EVENT as u16 {
                    continue;
                }
                let key = unsafe { record.Event.KeyEvent };
                if key.bKeyDown == 0 {
                    continue;
                }
                let ch = unsafe { char::from_u32(key.uChar.UnicodeChar as u32) };
                match ch.map(|value| value.to_ascii_lowercase()) {
                    Some('y') => {
                        eprintln!("] y");
                        return Ok(true);
                    }
                    Some('n') | Some('\u{1b}') => {
                        eprintln!("] n");
                        return Ok(false);
                    }
                    Some(value) if !value.is_control() => {
                        if !invalid_key {
                            eprint!(" wrong key; press y or n ");
                            let _ = io::stderr().flush();
                            invalid_key = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => return confirm_rescan_line(),
        };
    }
}

#[cfg(not(windows))]
fn confirm_rescan_keypress() -> Result<bool, CliError> {
    confirm_rescan_line()
}

fn confirm_rescan_line() -> Result<bool, CliError> {
    eprint!("Proceed with rescan? [y/N] auto-skip in 5s: ");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let answer = io::stdin()
            .read_line(&mut line)
            .map(|_| line.trim().to_ascii_lowercase());
        let _ = sender.send(answer);
    });

    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(answer)) => Ok(answer == "y" || answer == "yes"),
        Ok(Err(source)) => Err(CliError::Io {
            context: "failed to read rescan confirmation".to_string(),
            source,
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!();
            Ok(false)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(false),
    }
}
