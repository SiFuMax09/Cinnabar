//! `/schematic` and `//load` / `//save`, with the messages and permissions of FastAsyncWorldEdit.

use super::*;
use crate::config::CONFIG;
use crate::utils::HyphenatedUUID;
use cinnabar_schematic::{load_schematic, save_schematic, WorldEditClipboard};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::error;

const SCHEM_DIR: &str = "./schems";
const PAGE_SIZE: usize = 10;
const SUBCOMMANDS_USAGE: &str =
    "/schematic <clear|load|save|delete|list|unload|loadall|move|formats|show>";

/// The schematic each player loaded last, relative to the schematic directory. `/schematic unload`
/// and `/schematic move` work on it.
static LOADED: LazyLock<Mutex<HashMap<u128, String>>> = LazyLock::new(Default::default);

/// Forgets which file the clipboard of the player came from, because it was replaced
pub(super) fn forget_loaded(uuid: u128) {
    if let Ok(mut loaded) = LOADED.lock() {
        loaded.remove(&uuid);
    }
}

fn set_loaded(uuid: u128, name: &str) {
    if let Ok(mut loaded) = LOADED.lock() {
        loaded.insert(uuid, name.to_owned());
    }
}

fn loaded(uuid: u128) -> Option<String> {
    LOADED.lock().ok()?.get(&uuid).cloned()
}

#[derive(Debug, PartialEq)]
enum SchemError {
    InvalidName,
    NotFound,
    AlreadyExists,
    Io(String),
}

/// The directory of the schematics of the player
fn base_dir(player: &Player) -> PathBuf {
    if CONFIG.schemati {
        PathBuf::from(SCHEM_DIR).join(HyphenatedUUID(player.uuid).to_string())
    } else {
        PathBuf::from(SCHEM_DIR)
    }
}

/// Checks that a name is a relative path made of plain folders and a file name
fn sanitize_name(name: &str) -> Result<String, SchemError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SchemError::InvalidName);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | ' '))
    {
        return Err(SchemError::InvalidName);
    }
    for segment in name.split('/') {
        if segment.is_empty() || segment.starts_with('.') || segment.trim() != segment {
            return Err(SchemError::InvalidName);
        }
    }
    Ok(name.to_owned())
}

fn has_extension(name: &str) -> bool {
    name.ends_with(".schem") || name.ends_with(".schematic")
}

/// The files a name can stand for, in the order they are tried
fn candidates(name: &str) -> Vec<String> {
    if has_extension(name) {
        vec![name.to_owned()]
    } else {
        vec![format!("{name}.schem"), format!("{name}.schematic")]
    }
}

/// The file a clipboard is saved to. The format is always Sponge, which uses `.schem`.
fn save_name(name: &str) -> String {
    if name.ends_with(".schem") {
        name.to_owned()
    } else if let Some(stem) = name.strip_suffix(".schematic") {
        format!("{stem}.schem")
    } else {
        format!("{name}.schem")
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Format {
    Sponge,
}

/// The formats the server can read and write: (name, lookup names)
const FORMATS: &[(&str, &[&str])] = &[(
    "sponge_v2",
    &["sponge_v2", "sponge.2", "sponge", "schem", "fast"],
)];

fn parse_format(name: &str) -> Option<Format> {
    let name = name.to_ascii_lowercase();
    FORMATS
        .iter()
        .any(|(_, lookups)| lookups.contains(&name.as_str()))
        .then_some(Format::Sponge)
}

type SplitArgs = (Vec<String>, Vec<char>, Vec<(char, String)>);

/// Splits the arguments into plain arguments, flags and the values of the flags that take one
fn split_flags(
    raw: &[String],
    flags_with_value: &[char],
    known_flags: &[char],
) -> Result<SplitArgs, String> {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut values = Vec::new();
    let mut iter = raw.iter();
    while let Some(arg) = iter.next() {
        let is_flag = arg.len() > 1
            && arg.starts_with('-')
            && arg[1..].starts_with(|c: char| c.is_ascii_alphabetic());
        if !is_flag {
            positional.push(arg.clone());
            continue;
        }
        for flag in arg.chars().skip(1) {
            if !known_flags.contains(&flag) {
                return Err(format!("Unknown flag: {flag}"));
            }
            if flags_with_value.contains(&flag) {
                match iter.next() {
                    Some(value) => values.push((flag, value.clone())),
                    None => return Err(format!("Missing argument for flag {flag}")),
                }
            }
            flags.push(flag);
        }
    }
    Ok((positional, flags, values))
}

struct SchemEntry {
    name: String,
    modified: SystemTime,
}

fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<SchemEntry>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_entries(root, &path, out);
        } else if file_type.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !has_extension(&name) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(SchemEntry { name, modified });
        }
    }
}

fn list_entries(root: &Path) -> Vec<SchemEntry> {
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries);
    entries
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Sort {
    Name,
    OldestFirst,
    NewestFirst,
}

fn sort_entries(entries: &mut [SchemEntry], sort: Sort) {
    match sort {
        Sort::Name => entries.sort_by_key(|e| e.name.to_ascii_lowercase()),
        Sort::OldestFirst => entries.sort_by_key(|e| e.modified),
        Sort::NewestFirst => entries.sort_by_key(|e| std::cmp::Reverse(e.modified)),
    }
}

/// The number of pages and the range of the entries of a page (counted from 1)
fn page_range(len: usize, page: usize) -> Result<(usize, std::ops::Range<usize>), usize> {
    let pages = len.div_ceil(PAGE_SIZE).max(1);
    if page == 0 || page > pages {
        return Err(pages);
    }
    let start = (page - 1) * PAGE_SIZE;
    Ok((pages, start..(start + PAGE_SIZE).min(len)))
}

fn load_from(root: &Path, name: &str) -> Result<(String, WorldEditClipboard), SchemError> {
    let name = sanitize_name(name)?;
    for candidate in candidates(&name) {
        let path = root.join(&candidate);
        if !path.is_file() {
            continue;
        }
        return match load_schematic(&path) {
            Ok(clipboard) => Ok((candidate, clipboard)),
            Err(e) => {
                error!("There was an error loading a schematic:");
                error!("{}", e);
                Err(SchemError::Io(e.to_string()))
            }
        };
    }
    Err(SchemError::NotFound)
}

fn save_to(
    root: &Path,
    name: &str,
    clipboard: &WorldEditClipboard,
    overwrite: bool,
) -> Result<String, SchemError> {
    let name = save_name(&sanitize_name(name)?);
    let path = root.join(&name);
    if path.exists() && !overwrite {
        return Err(SchemError::AlreadyExists);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SchemError::Io(e.to_string()))?;
    }
    save_schematic(&path, clipboard).map_err(|e| {
        error!("There was an error saving a schematic: ");
        error!("{:?}", e);
        SchemError::Io(e.to_string())
    })?;
    Ok(name)
}

fn delete_from(root: &Path, name: &str) -> Result<String, SchemError> {
    let name = sanitize_name(name)?;
    for candidate in candidates(&name) {
        let path = root.join(&candidate);
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| SchemError::Io(e.to_string()))?;
            return Ok(candidate);
        }
    }
    Err(SchemError::NotFound)
}

fn delete_all(root: &Path) -> usize {
    list_entries(root)
        .iter()
        .filter(|e| std::fs::remove_file(root.join(&e.name)).is_ok())
        .count()
}

fn move_to(root: &Path, from: &str, dir: &str) -> Result<String, SchemError> {
    let dir = sanitize_name(dir)?;
    let source = root.join(from);
    if !source.is_file() {
        return Err(SchemError::NotFound);
    }
    let file = from.rsplit('/').next().unwrap_or(from);
    let target_name = format!("{dir}/{file}");
    let target = root.join(&target_name);
    if target.exists() {
        return Err(SchemError::AlreadyExists);
    }
    std::fs::create_dir_all(root.join(&dir)).map_err(|e| SchemError::Io(e.to_string()))?;
    std::fs::rename(source, target).map_err(|e| SchemError::Io(e.to_string()))?;
    Ok(target_name)
}

fn error_message(err: &SchemError, name: &str) -> String {
    match err {
        SchemError::InvalidName => "Filename is invalid".to_owned(),
        SchemError::NotFound => format!("Schematic {name} does not exist!"),
        SchemError::AlreadyExists => {
            "That schematic already exists. Use the -f flag to overwrite it.".to_owned()
        }
        SchemError::Io(_) => {
            "There was an error accessing the schematic. Check console for more details.".to_owned()
        }
    }
}

fn usage(player: &Player, text: &str) {
    player.send_error_message(&format!("Usage: {text}"));
}

fn may(player: &Player, sub: &str) -> bool {
    if player.has_permission(&format!("worldedit.schematic.{sub}")) {
        return true;
    }
    // The permissions of `//load` and `//save`
    matches!(sub, "load" | "save") && player.has_permission(&format!("worldedit.clipboard.{sub}"))
}

pub(super) fn execute_schematic(ctx: CommandExecuteContext<'_>) {
    let raw: Vec<String> = ctx
        .arguments
        .iter()
        .map(|a| a.unwrap_string().clone())
        .filter(|a| !a.is_empty())
        .collect();
    let player = ctx.player;
    let Some(sub) = raw.first() else {
        usage(player, SUBCOMMANDS_USAGE);
        return;
    };
    let sub = sub.to_ascii_lowercase();
    let rest = &raw[1..];
    let known = [
        "clear", "load", "save", "delete", "list", "unload", "loadall", "move", "formats", "show",
    ];
    if !known.contains(&sub.as_str()) {
        usage(player, SUBCOMMANDS_USAGE);
        return;
    }
    if !may(player, &sub) {
        player.send_no_permission_message();
        return;
    }
    match sub.as_str() {
        "clear" => {
            forget_loaded(player.uuid);
            player.worldedit_clipboard = None;
            player.send_worldedit_message("Clipboard cleared.");
        }
        "load" => sub_load(player, rest),
        "save" => sub_save(player, rest),
        "delete" => sub_delete(player, rest),
        "list" => sub_list(player, rest),
        "unload" => sub_unload(player, rest),
        "loadall" => sub_loadall(player, rest),
        "move" => sub_move(player, rest),
        "formats" => send_formats(player),
        _ => player.send_error_message(
            "The schematic preview needs the web interface, which is not available on this server.",
        ),
    }
}

/// The text of `/schematic formats`
fn send_formats(player: &Player) {
    player.send_worldedit_message("Available clipboard formats (Name: Lookup names)");
    for (name, lookups) in FORMATS {
        player.send_worldedit_message(&format!("{name}: {}", lookups.join(", ")));
    }
}

/// Splits `[format] <file>` and checks the format
fn format_and_file(player: &Player, positional: &[String], usage_text: &str) -> Option<String> {
    match positional {
        [file] => Some(file.clone()),
        [format, file] => {
            if parse_format(format).is_none() {
                player.send_error_message(&format!("Unknown schematic format: {format}."));
                return None;
            }
            Some(file.clone())
        }
        _ => {
            usage(player, usage_text);
            None
        }
    }
}

fn parse_or_report(
    player: &Player,
    raw: &[String],
    flags_with_value: &[char],
    known: &[char],
) -> Option<SplitArgs> {
    match split_flags(raw, flags_with_value, known) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            player.send_error_message(&e);
            None
        }
    }
}

pub(super) fn load_command(player: &mut Player, name: &str) {
    let root = base_dir(player);
    match load_from(&root, name) {
        Ok((file, clipboard)) => {
            player.worldedit_clipboard = Some(clipboard);
            super::clipboard_cmds::set_origin(player.uuid, None);
            set_loaded(player.uuid, &file);
            player.send_worldedit_message(&format!("{name} loaded. Paste it with //paste"));
        }
        Err(err) => player.send_error_message(&error_message(&err, name)),
    }
}

pub(super) fn save_command(player: &mut Player, name: &str, overwrite: bool) {
    let Some(clipboard) = &player.worldedit_clipboard else {
        player.send_error_message("Your clipboard is empty. Use //copy first.");
        return;
    };
    let root = base_dir(player);
    match save_to(&root, name, clipboard, overwrite) {
        Ok(_) => player.send_worldedit_message(&format!("{name} saved.")),
        Err(err) => player.send_error_message(&error_message(&err, name)),
    }
}

/// `//load <name>`
pub(super) fn execute_load(ctx: CommandExecuteContext<'_>) {
    let name = ctx.arguments[0].unwrap_string().clone();
    load_command(ctx.player, &name);
}

/// `//save [-f] <name>`
pub(super) fn execute_save(ctx: CommandExecuteContext<'_>) {
    let name = ctx.arguments[0].unwrap_string().clone();
    let overwrite = ctx.has_flag('f');
    save_command(ctx.player, &name, overwrite);
}

fn sub_load(player: &mut Player, rest: &[String]) {
    let Some((positional, _, _)) = parse_or_report(player, rest, &[], &[]) else {
        return;
    };
    if let Some(file) =
        format_and_file(player, &positional, "/schematic load [<format>] <filename>")
    {
        load_command(player, &file);
    }
}

fn sub_save(player: &mut Player, rest: &[String]) {
    let Some((positional, flags, _)) = parse_or_report(player, rest, &[], &['f']) else {
        return;
    };
    if let Some(file) = format_and_file(
        player,
        &positional,
        "/schematic save [-f] [<format>] <filename>",
    ) {
        save_command(player, &file, flags.contains(&'f'));
    }
}

fn sub_delete(player: &mut Player, rest: &[String]) {
    let [name] = rest else {
        usage(player, "/schematic delete <filename|*>");
        return;
    };
    let root = base_dir(player);
    if name == "*" {
        let count = delete_all(&root);
        player.send_worldedit_message(&format!("{count} schematic(s) have been deleted."));
        return;
    }
    match delete_from(&root, name) {
        Ok(_) => {
            if loaded(player.uuid).is_some_and(|l| candidates(name).contains(&l)) {
                forget_loaded(player.uuid);
            }
            player.send_worldedit_message(&format!("{name} has been deleted."));
        }
        Err(SchemError::Io(_)) => {
            player.send_error_message(&format!("Deletion of {name} failed! Is it read-only?"))
        }
        Err(err) => player.send_error_message(&error_message(&err, name)),
    }
}

fn sub_list(player: &mut Player, rest: &[String]) {
    let text = "/schematic list [-dnp] [-f <format>] [global|mine|<filter>] [<page>]";
    let Some((positional, flags, values)) =
        parse_or_report(player, rest, &['p', 'f'], &['d', 'n', 'p', 'f'])
    else {
        return;
    };
    let mut global = false;
    let mut filter: Option<String> = None;
    let mut page: Option<usize> = None;
    for arg in &positional {
        match arg.to_ascii_lowercase().as_str() {
            "global" => global = true,
            "mine" => global = false,
            _ => {
                if let Ok(number) = arg.parse::<usize>() {
                    page = Some(number);
                } else if filter.is_none() {
                    filter = Some(arg.to_ascii_lowercase());
                } else {
                    usage(player, text);
                    return;
                }
            }
        }
    }
    if let Some((_, value)) = values.iter().find(|(f, _)| *f == 'p') {
        match value.parse::<usize>() {
            Ok(number) => page = Some(number),
            Err(_) => {
                player.send_error_message(&format!("Not a number: {value}"));
                return;
            }
        }
    }
    if let Some((_, value)) = values.iter().find(|(f, _)| *f == 'f') {
        if parse_format(value).is_none() {
            player.send_error_message(&format!("Unknown schematic format: {value}."));
            return;
        }
    }
    let sort = if flags.contains(&'d') {
        Sort::OldestFirst
    } else if flags.contains(&'n') {
        Sort::NewestFirst
    } else {
        Sort::Name
    };

    let root = if global {
        PathBuf::from(SCHEM_DIR)
    } else {
        base_dir(player)
    };
    let mut entries = list_entries(&root);
    if let Some(filter) = &filter {
        entries.retain(|e| e.name.to_ascii_lowercase().contains(filter.as_str()));
    }
    if entries.is_empty() {
        player.send_error_message("No schematics found.");
        return;
    }
    sort_entries(&mut entries, sort);
    let page = page.unwrap_or(1);
    match page_range(entries.len(), page) {
        Ok((pages, range)) => {
            player.send_worldedit_message(&format!(
                "Available schematics (Filename: Format) [{page}/{pages}]"
            ));
            for entry in &entries[range] {
                player.send_worldedit_message(&format!("{}: Sponge (.schem)", entry.name));
            }
        }
        Err(pages) => {
            player.send_error_message(&format!("The page number must be between 1 and {pages}."))
        }
    }
}

fn sub_unload(player: &mut Player, rest: &[String]) {
    if rest.len() > 1 {
        usage(player, "/schematic unload [<filename>]");
        return;
    }
    let current = loaded(player.uuid);
    if let Some(name) = rest.first() {
        // Only the schematic that is loaded can be unloaded
        let matches = current
            .as_ref()
            .is_some_and(|c| candidates(name).contains(c));
        if !matches {
            player.send_error_message(&format!("Schematic {name} does not exist!"));
            return;
        }
    }
    if player.worldedit_clipboard.is_none() {
        player.send_error_message("Your clipboard is empty. Use //copy first.");
        return;
    }
    forget_loaded(player.uuid);
    player.worldedit_clipboard = None;
    let name = rest.first().cloned().or(current).unwrap_or_default();
    player.send_worldedit_message(&format!("{name} has been unloaded."));
}

fn sub_loadall(player: &mut Player, rest: &[String]) {
    let Some((positional, flags, _)) = parse_or_report(player, rest, &[], &['r']) else {
        return;
    };
    let filter = match positional.as_slice() {
        [] => None,
        [one] if parse_format(one).is_some() => None,
        [one] => Some(one.to_ascii_lowercase()),
        [format, filter] if parse_format(format).is_some() => Some(filter.to_ascii_lowercase()),
        _ => {
            usage(player, "/schematic loadall [-r] [<format>] [<filter>]");
            return;
        }
    };
    let root = base_dir(player);
    let mut entries = list_entries(&root);
    if !flags.contains(&'r') {
        entries.retain(|e| !e.name.contains('/'));
    }
    if let Some(filter) = &filter {
        entries.retain(|e| e.name.to_ascii_lowercase().contains(filter.as_str()));
    }
    sort_entries(&mut entries, Sort::Name);
    // The server has one clipboard per player, so the last schematic is the one that stays
    let mut count = 0;
    for entry in &entries {
        if let Ok((file, clipboard)) = load_from(&root, &entry.name) {
            player.worldedit_clipboard = Some(clipboard);
            set_loaded(player.uuid, &file);
            count += 1;
        }
    }
    if count == 0 {
        player.send_error_message("No schematics found.");
        return;
    }
    super::clipboard_cmds::set_origin(player.uuid, None);
    player.send_worldedit_message(&format!(
        "{count} schematic(s) loaded. Paste the current one with //paste"
    ));
}

fn sub_move(player: &mut Player, rest: &[String]) {
    let [dir] = rest else {
        usage(player, "/schematic move <directory>");
        return;
    };
    let Some(current) = loaded(player.uuid) else {
        player.send_error_message("You have no schematic loaded.");
        return;
    };
    let root = base_dir(player);
    match move_to(&root, &current, dir) {
        Ok(target) => {
            set_loaded(player.uuid, &target);
            player.send_worldedit_message(&format!("{current} has been moved to {dir}."));
        }
        Err(err) => player.send_error_message(&error_message(&err, &current)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cinnabar_schem_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clipboard() -> WorldEditClipboard {
        WorldEditClipboard {
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
            size_x: 2,
            size_y: 1,
            size_z: 2,
            data: vec![0; 4],
            block_entities: FxHashMap::default(),
        }
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(sanitize_name("house"), Ok("house".to_owned()));
        assert_eq!(
            sanitize_name("dir/house.schem"),
            Ok("dir/house.schem".to_owned())
        );
        for bad in [
            "", "../x", "a/../x", "/abs", "a//b", ".hidden", "a\\b", "a/.b", "x;y",
        ] {
            assert_eq!(sanitize_name(bad), Err(SchemError::InvalidName), "{bad}");
        }
    }

    #[test]
    fn extensions() {
        assert_eq!(candidates("a"), vec!["a.schem", "a.schematic"]);
        assert_eq!(candidates("a.schem"), vec!["a.schem"]);
        assert_eq!(save_name("a"), "a.schem");
        assert_eq!(save_name("a.schem"), "a.schem");
        assert_eq!(save_name("a.schematic"), "a.schem");
    }

    #[test]
    fn formats_are_looked_up_by_any_name() {
        assert_eq!(parse_format("Sponge"), Some(Format::Sponge));
        assert_eq!(parse_format("schem"), Some(Format::Sponge));
        assert_eq!(parse_format("mcedit"), None);
    }

    #[test]
    fn flags_are_split() {
        let raw: Vec<String> = ["-dn", "-p", "3", "filter", "-5"]
            .map(String::from)
            .to_vec();
        let (positional, flags, values) = split_flags(&raw, &['p'], &['d', 'n', 'p']).unwrap();
        assert_eq!(positional, vec!["filter", "-5"]);
        assert_eq!(flags, vec!['d', 'n', 'p']);
        assert_eq!(values, vec![('p', "3".to_owned())]);
        assert!(split_flags(&["-x".to_owned()], &[], &['d']).is_err());
        assert!(split_flags(&["-p".to_owned()], &['p'], &['p']).is_err());
    }

    #[test]
    fn pages() {
        assert_eq!(page_range(0, 1), Ok((1, 0..0)));
        assert_eq!(page_range(25, 3), Ok((3, 20..25)));
        assert_eq!(page_range(25, 4), Err(3));
        assert_eq!(page_range(25, 0), Err(3));
    }

    #[test]
    fn save_load_delete_roundtrip() {
        let root = temp_root("roundtrip");
        let cb = clipboard();
        assert_eq!(
            save_to(&root, "dir/house", &cb, false),
            Ok("dir/house.schem".to_owned())
        );
        assert_eq!(
            save_to(&root, "dir/house", &cb, false),
            Err(SchemError::AlreadyExists)
        );
        assert!(save_to(&root, "dir/house", &cb, true).is_ok());

        let (file, loaded) = load_from(&root, "dir/house").unwrap();
        assert_eq!(file, "dir/house.schem");
        assert_eq!((loaded.size_x, loaded.size_y, loaded.size_z), (2, 1, 2));
        assert_eq!(
            load_from(&root, "missing").unwrap_err(),
            SchemError::NotFound
        );
        assert_eq!(
            load_from(&root, "../x").unwrap_err(),
            SchemError::InvalidName
        );

        let names: Vec<_> = list_entries(&root).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["dir/house.schem"]);

        assert_eq!(
            move_to(&root, "dir/house.schem", "other"),
            Ok("other/house.schem".to_owned())
        );
        assert_eq!(delete_from(&root, "dir/house"), Err(SchemError::NotFound));
        assert_eq!(
            delete_from(&root, "other/house"),
            Ok("other/house.schem".to_owned())
        );
        assert!(save_to(&root, "a", &cb, false).is_ok());
        assert!(save_to(&root, "b", &cb, false).is_ok());
        assert_eq!(delete_all(&root), 2);
        assert!(list_entries(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
