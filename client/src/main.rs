mod api;
mod config;
mod note;
mod notes_tui;
mod sync;
mod watch;

use std::io::{IsTerminal, Write};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use api::{Api, Entry, Outcome};
use config::{Config, State};
use sync::{BoxError, Choice, Syncer};

const USAGE: &str = "\
mapaki — sync a folder with a mapaki server

USAGE:
    mapaki pair <url> <code> [--name NAME] [--folder DIR]
                              pair with a server (code from `mapaki-daemon pair-code`)
    mapaki run                stay running: sync on changes either side (what the service runs)
    mapaki sync               sync once now, asking about any conflicts
    mapaki send [-i] <file>...
                              upload files straight to the server (also: `mapaki <file>...`);
                              a name already taken there gets a new name, like `notes-2.txt`.
                              -i asks first: overwrite (as a new revision), rename, or cancel
    mapaki notes              browse, search and edit notes (.note files) in a terminal UI
    mapaki ls                 list the server's files and whether each is in sync here
    mapaki status             show servers, folder and pending conflicts
    mapaki conflicts          list conflicts waiting on you
    mapaki resolve <file> local|remote|both
                              settle a conflict: keep yours, keep theirs, or keep both
    mapaki history <file>     list a file's revisions
    mapaki restore <file> <revision>
                              bring back an old revision (synced as a new edit)
    mapaki url list|add <url> [--first]|remove <url>
                              server addresses, tried in order (LAN first)
    mapaki folder [DIR]       show or change the synced folder
    mapaki --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("pair") => cmd_pair(&args[1..]),
        Some("run") => need_config().and_then(watch::run),
        Some("sync") => cmd_sync(),
        Some("ls" | "list") => cmd_ls(),
        Some("status") => cmd_status(),
        Some("conflicts") => cmd_conflicts(),
        Some("resolve") => cmd_resolve(&args[1..]),
        Some("history") => cmd_history(&args[1..]),
        Some("restore") => cmd_restore(&args[1..]),
        Some("url") => cmd_url(&args[1..]),
        Some("folder") => cmd_folder(&args[1..]),
        Some("send") => cmd_send(&args[1..]),
        Some("notes") => cmd_notes(),
        Some("-i" | "--ask") => cmd_send(&args),
        Some(path) if Path::new(path).is_file() => cmd_send(&args),
        Some("--version" | "-V" | "version") => {
            println!("mapaki {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mapaki: {e}");
            ExitCode::FAILURE
        }
    }
}

fn need_config() -> Result<Config, BoxError> {
    Config::load().ok_or_else(|| "not paired yet — run `mapaki pair <url> <code>` first".into())
}

/// First configured URL that answers, in priority order.
pub fn connect(cfg: &Config) -> Result<Api, BoxError> {
    for url in &cfg.urls {
        if api::reachable(url) {
            return Ok(Api::new(url, &cfg.token));
        }
    }
    Err(format!("no mapaki server reachable (tried {})", cfg.urls.join(", ")).into())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "desktop".into())
}

fn cmd_pair(args: &[String]) -> Result<(), BoxError> {
    let (Some(url), Some(code)) = (args.first(), args.get(1)) else {
        return Err("usage: mapaki pair <url> <code> [--name NAME] [--folder DIR]".into());
    };
    let url = api::normalize_url(url);
    let name = flag(args, "--name").unwrap_or_else(hostname);
    let token = api::pair(&url, &code.trim().to_uppercase(), &name)?;
    let previous = Config::load();
    let folder = flag(args, "--folder")
        .map(PathBuf::from)
        .or_else(|| previous.as_ref().map(|c| c.folder.clone()))
        .unwrap_or_else(config::default_folder);
    let mut urls = vec![url.clone()];
    if let Some(prev) = previous {
        urls.extend(prev.urls.into_iter().filter(|u| *u != url));
    }
    let cfg = Config { urls, token, folder };
    cfg.save()?;
    // A new pairing may be a different server; never reuse another one's bases.
    State::reset()?;
    println!("paired as \"{name}\" with {url}");
    println!("syncing folder: {}", cfg.folder.display());
    println!("run `mapaki sync` now, and enable the background service with:");
    println!("    systemctl --user enable --now mapaki");
    Ok(())
}

fn prompt_choice(name: &str, remote: &Entry) -> Option<Choice> {
    if remote.deleted {
        println!("\n\"{name}\" was deleted on the server but changed here.");
    } else {
        println!("\n\"{name}\" changed both here and on another device, and the changes can't be merged.");
    }
    loop {
        print!("keep [l]ocal, [r]emote, [b]oth (rename yours), or [s]kip for now? ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).ok()? == 0 {
            return None;
        }
        match line.trim().to_lowercase().as_str() {
            "l" | "local" => return Some(Choice::Local),
            "r" | "remote" => return Some(Choice::Remote),
            "b" | "both" => return Some(Choice::Both),
            "s" | "skip" => return None,
            _ => {}
        }
    }
}

fn cmd_sync() -> Result<(), BoxError> {
    let cfg = need_config()?;
    let api = connect(&cfg)?;
    let mut state = State::load();
    let interactive = std::io::stdin().is_terminal();
    let mut decide = |name: &str, remote: &Entry| if interactive { prompt_choice(name, remote) } else { None };
    let report = Syncer::new(&api, &cfg.folder, &mut state, &mut decide).run()?;
    for e in &report.errors {
        eprintln!("  error: {e}");
    }
    println!("{} ({})", report.summary(), api.base);
    if !report.conflicts.is_empty() {
        println!("settle conflicts with `mapaki resolve <file> local|remote|both`");
    }
    Ok(())
}

enum SendChoice {
    Overwrite,
    Rename,
    Cancel,
}

fn prompt_send(name: &str) -> SendChoice {
    println!("\n\"{name}\" is already on the server.");
    loop {
        print!("[o]verwrite (the current version stays in history), [r]ename, or [c]ancel? [r] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => return SendChoice::Cancel,
            Ok(_) => {}
        }
        match line.trim().to_lowercase().as_str() {
            "o" | "overwrite" => return SendChoice::Overwrite,
            "" | "r" | "rename" => return SendChoice::Rename,
            "c" | "cancel" => return SendChoice::Cancel,
            _ => {}
        }
    }
}

fn cmd_send(args: &[String]) -> Result<(), BoxError> {
    let ask = args.iter().any(|a| a == "-i" || a == "--ask");
    let paths: Vec<&String> = args.iter().filter(|a| *a != "-i" && *a != "--ask").collect();
    if paths.is_empty() {
        return Err("usage: mapaki send [-i] <file>...".into());
    }
    if ask && !std::io::stdin().is_terminal() {
        return Err("-i needs a terminal to ask in; leave it off to rename automatically".into());
    }
    let cfg = need_config()?;
    let api = connect(&cfg)?;
    let mut state = State::load();
    // Only -i needs to know what's already there; without it the server picks a free name.
    let mut live: HashMap<String, Entry> = if ask {
        api.changes(0)?.entries.into_iter().filter(|e| !e.deleted).map(|e| (e.name.clone(), e)).collect()
    } else {
        HashMap::new()
    };
    let mut failed = 0;
    for path in paths {
        match send_one(&api, &cfg, &mut state, &mut live, Path::new(path), ask) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("mapaki: {path}: {e}");
                failed += 1;
            }
        }
    }
    state.save()?;
    if failed > 0 {
        return Err(format!("{failed} file(s) not sent").into());
    }
    Ok(())
}

fn send_one(
    api: &Api,
    cfg: &Config,
    state: &mut State,
    live: &mut HashMap<String, Entry>,
    path: &Path,
    ask: bool,
) -> Result<String, BoxError> {
    let name = path.file_name().and_then(|n| n.to_str()).ok_or("not a file name")?;
    if !sync::is_synced_name(name) {
        return Err("hidden and temporary files aren't synced".into());
    }
    let bytes = std::fs::read(path)?;
    let (entry, overwrote) = match live.get(name).cloned() {
        Some(remote) if ask => match prompt_send(name) {
            SendChoice::Cancel => return Ok(format!("skipped {name}")),
            SendChoice::Rename => (api.create(name, &bytes)?, false),
            // Based on the head we just saw, so it's a fast-forward and the
            // old content stays in `mapaki history`.
            SendChoice::Overwrite => match api.put(&remote.id, &bytes, Some(&remote.head), false)? {
                Outcome::Ok((_, e)) => (e, true),
                Outcome::Conflict(_) => {
                    return Err("it changed on the server while you were deciding; run it again".into())
                }
            },
        },
        _ => (api.create(name, &bytes)?, false),
    };
    live.insert(entry.name.clone(), entry.clone());
    let mut msg = if overwrote {
        format!("sent {name} (new revision; `mapaki history {name}` has the old one)")
    } else if entry.name != name {
        format!("sent {name} as {}", entry.name)
    } else {
        format!("sent {name}")
    };
    if !sync::mirror_sent(&cfg.folder, state, &entry, &bytes)? {
        msg.push_str(&format!(" (the local {} has unsynced edits; `mapaki sync` will reconcile)", entry.name));
    }
    Ok(msg)
}

fn cmd_notes() -> Result<(), BoxError> {
    if !std::io::stdout().is_terminal() {
        return Err("`mapaki notes` needs a terminal".into());
    }
    notes_tui::run(need_config()?)
}

fn cmd_status() -> Result<(), BoxError> {
    let cfg = need_config()?;
    let state = State::load();
    println!("folder: {}", cfg.folder.display());
    println!("servers (tried in order):");
    for url in &cfg.urls {
        let up = if api::reachable(url) { "reachable" } else { "unreachable" };
        println!("  {url}  [{up}]");
    }
    println!("tracked files: {}", state.files.len());
    println!("conflicts: {}", state.conflicts.len());
    Ok(())
}

fn cmd_ls() -> Result<(), BoxError> {
    let cfg = need_config()?;
    let state = State::load();
    // Offline, compare against what this computer last synced instead.
    let (remote, offline) = match connect(&cfg).and_then(|api| Ok(api.changes(0)?)) {
        Ok(changes) => (changes.entries, false),
        Err(_) => {
            let known = state.files.iter().map(|(id, f)| Entry {
                id: id.clone(),
                name: f.name.clone(),
                head: f.base.clone(),
                hash: f.hash.clone(),
                size: f.size,
                deleted: false,
            });
            let mut known: Vec<Entry> = known.collect();
            known.sort_by(|a, b| a.name.cmp(&b.name));
            (known, true)
        }
    };
    let listed = sync::listing(&cfg.folder, &state, &remote);
    if offline {
        println!("server unreachable: showing what this computer last synced\n");
    }
    if listed.is_empty() {
        println!("no files yet");
        return Ok(());
    }
    let width = listed.iter().map(|l| l.name.chars().count()).max().unwrap_or(0).clamp(4, 48);
    println!("{:<width$}  {:>8}  HERE", "NAME", "SIZE");
    for l in &listed {
        let name: String = if l.name.chars().count() > width {
            l.name.chars().take(width - 1).chain(['…']).collect()
        } else {
            l.name.clone()
        };
        println!("{name:<width$}  {:>8}  {}", human_size(l.size), l.here.label());
    }
    let pending = listed.iter().filter(|l| l.here != sync::Here::Synced).count();
    if pending > 0 && !config::service_running() {
        println!("\n{pending} not in sync; `mapaki sync` sorts them out");
    }
    Ok(())
}

pub fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

fn cmd_conflicts() -> Result<(), BoxError> {
    let state = State::load();
    if state.conflicts.is_empty() {
        println!("no conflicts");
    }
    let mut names: Vec<&String> = state.conflicts.keys().collect();
    names.sort();
    for name in names {
        let c = &state.conflicts[name];
        let why = if c.remote_deleted { "deleted on server, changed here" } else { "changed on both sides" };
        println!("{name}  ({why})");
    }
    Ok(())
}

fn cmd_resolve(args: &[String]) -> Result<(), BoxError> {
    let (Some(name), Some(choice)) = (args.first(), args.get(1)) else {
        return Err("usage: mapaki resolve <file> local|remote|both".into());
    };
    let choice = match choice.as_str() {
        "local" | "l" => Choice::Local,
        "remote" | "r" => Choice::Remote,
        "both" | "b" => Choice::Both,
        _ => return Err("choose one of: local, remote, both".into()),
    };
    let cfg = need_config()?;
    let api = connect(&cfg)?;
    let mut state = State::load();
    let report = sync::resolve(&api, &cfg.folder, &mut state, name, choice)?;
    println!("resolved \"{name}\": {}", report.summary());
    Ok(())
}

fn tracked_id(state: &State, name: &str) -> Result<String, BoxError> {
    state
        .files
        .iter()
        .find(|(_, f)| f.name == name)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| format!("\"{name}\" isn't a synced file (run `mapaki sync` first?)").into())
}

fn cmd_history(args: &[String]) -> Result<(), BoxError> {
    let name = args.first().ok_or("usage: mapaki history <file>")?;
    let cfg = need_config()?;
    let api = connect(&cfg)?;
    let state = State::load();
    let id = tracked_id(&state, name)?;
    let current = &state.files[&id].base;
    for rev in api.history(&id)?.iter().rev() {
        let marker = if &rev.id == current { "*" } else { " " };
        let what = if rev.deleted {
            "deleted".to_string()
        } else if rev.parents.len() > 1 {
            format!("merge, {} bytes", rev.size)
        } else {
            format!("{} bytes", rev.size)
        };
        println!("{marker} {}  {}  {:<16} {what}  ({})", rev.id, format_time(rev.at), rev.device, rev.name);
    }
    Ok(())
}

fn cmd_restore(args: &[String]) -> Result<(), BoxError> {
    let (Some(name), Some(rev)) = (args.first(), args.get(1)) else {
        return Err("usage: mapaki restore <file> <revision>".into());
    };
    let cfg = need_config()?;
    let api = connect(&cfg)?;
    let state = State::load();
    let id = tracked_id(&state, name)?;
    let bytes = api.content(&id, Some(rev))?;
    std::fs::write(cfg.folder.join(name), bytes)?;
    println!("restored \"{name}\" to {rev}; syncing it as a new edit");
    cmd_sync()
}

fn cmd_url(args: &[String]) -> Result<(), BoxError> {
    let mut cfg = need_config()?;
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("add"), Some(url)) => {
            let url = api::normalize_url(url);
            cfg.urls.retain(|u| *u != url);
            if args.iter().any(|a| a == "--first") {
                cfg.urls.insert(0, url);
            } else {
                cfg.urls.push(url);
            }
        }
        (Some("remove"), Some(url)) => {
            let url = api::normalize_url(url);
            if cfg.urls.len() == 1 && cfg.urls[0] == url {
                return Err("can't remove the only server address".into());
            }
            cfg.urls.retain(|u| *u != url);
        }
        (None | Some("list"), _) => {}
        _ => return Err("usage: mapaki url list|add <url> [--first]|remove <url>".into()),
    }
    cfg.save()?;
    for (i, url) in cfg.urls.iter().enumerate() {
        println!("{}. {url}", i + 1);
    }
    Ok(())
}

fn cmd_folder(args: &[String]) -> Result<(), BoxError> {
    let mut cfg = need_config()?;
    if let Some(dir) = args.first() {
        let dir = PathBuf::from(dir);
        let dir = if dir.is_absolute() { dir } else { std::env::current_dir()?.join(dir) };
        cfg.folder = dir;
        cfg.save()?;
        // Bases recorded against the old folder's files don't apply to the new one.
        State::reset()?;
        println!("now syncing {} (restart the service: systemctl --user restart mapaki)", cfg.folder.display());
    } else {
        println!("{}", cfg.folder.display());
    }
    Ok(())
}

/// UTC `YYYY-MM-DD HH:MM` without pulling in a date crate.
fn format_time(unix: i64) -> String {
    let (date, time) = civil(unix);
    format!("{date} {}Z", &time[..5])
}

/// UTC (`YYYY-MM-DD`, `HH:MM:SS`) for a unix time.
pub fn civil(unix: i64) -> (String, String) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_time_is_utc_civil_date() {
        assert_eq!(super::format_time(0), "1970-01-01 00:00Z");
        assert_eq!(super::format_time(1_763_769_600), "2025-11-22 00:00Z");
    }
}
