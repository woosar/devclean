use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Wrap},
};

use crate::data_structures::{
    App, CleanEvent, Focus, Language, Mode, Platform, Relic, Root, RootKind, ScanEvent, ScanReport, Tone, TreeNode,
};

// ======================================================================== platform

pub fn home_dir() -> Option<PathBuf> {
    let vars = if cfg!(windows) { ["USERPROFILE", "HOME"] } else { ["HOME", "USERPROFILE"] };
    vars.iter().filter_map(std::env::var_os).map(PathBuf::from).find(|p| p.is_dir())
}

/// Home first, then every drive / filesystem / mount the platform exposes.
pub fn discover_roots(platform: &Platform) -> Vec<Root> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(Root { path: home, label: "Home".into(), kind: RootKind::Home, fs_type: None, volume: None });
    }
    roots.extend(platform_roots(platform));
    roots
}

#[cfg(windows)]
fn platform_roots(_platform: &Platform) -> Vec<Root> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

    // SAFETY: no arguments, returns a bitmask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let letter = (b'A' + i) as char;
            let path = format!("{letter}:\\");
            let wide = wide(Path::new(&path));
            // SAFETY: `wide` is a NUL-terminated UTF-16 string.
            let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
            let kind = match drive_type {
                2 | 5 => RootKind::Removable,
                4 => RootKind::Network,
                _ => RootKind::Drive,
            };
            // Only query local media: network shares and empty optical drives can stall.
            let (volume, fs_type) = if matches!(drive_type, 2 | 3 | 6) { volume_info(&wide) } else { (None, None) };
            Root { path: PathBuf::from(&path), label: format!("{letter}:"), kind, fs_type, volume }
        })
        .collect()
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn volume_info(root: &[u16]) -> (Option<String>, Option<String>) {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

    let mut name = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: buffers are sized as declared; optional out-params are null.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return (None, None);
    }
    let text = |buf: &[u16]| {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.is_empty())
    };
    (text(&name), text(&fs))
}

#[cfg(not(windows))]
fn platform_roots(platform: &Platform) -> Vec<Root> {
    const IGNORED_PREFIXES: [&str; 3] = ["/mnt/wsl", "/mnt/wslg", "/usr/lib/wsl"];
    const MOUNT_PARENTS: [&str; 3] = ["/mnt/", "/media/", "/run/media/"];

    let mounts: Vec<(String, String, String, String)> = fs::read_to_string("/proc/mounts")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(unescape_mount);
            Some((fields.next()?, fields.next()?, fields.next()?, fields.next()?))
        })
        .collect();

    let root_fs = mounts.iter().rev().find(|(_, mp, _, _)| mp == "/").map(|(_, _, fs, _)| fs.clone());
    let mut roots = vec![Root {
        path: PathBuf::from("/"),
        label: "/".into(),
        kind: RootKind::Filesystem,
        fs_type: root_fs,
        volume: None,
    }];

    let mut seen = std::collections::HashSet::new();
    let mut extra: Vec<Root> = mounts
        .into_iter()
        .filter(|(_, mp, _, _)| MOUNT_PARENTS.iter().any(|p| mp.starts_with(p)))
        .filter(|(_, mp, _, _)| !IGNORED_PREFIXES.iter().any(|p| mp.starts_with(p)))
        .filter(|(_, mp, _, _)| seen.insert(mp.clone()))
        .map(|(device, mount_point, fs_type, options)| {
            let windows_drive = matches!(fs_type.as_str(), "9p" | "drvfs" | "virtiofs");
            let kind = match fs_type.as_str() {
                _ if windows_drive && matches!(platform, Platform::Wsl { .. }) => RootKind::Drive,
                "cifs" | "smb3" | "nfs" | "nfs4" | "sshfs" | "fuse.sshfs" => RootKind::Network,
                _ if mount_point.starts_with("/media/") || mount_point.starts_with("/run/media/") => {
                    RootKind::Removable
                }
                _ => RootKind::Mount,
            };
            // WSL drive mounts carry the Windows path either as device or as `path=C:\` option.
            let volume = options
                .split([',', ';'])
                .find_map(|o| o.strip_prefix("path="))
                .map(str::to_owned)
                .or_else(|| windows_drive.then(|| device.clone()))
                .filter(|v| v.contains(':'));
            Root { label: mount_point.clone(), path: PathBuf::from(mount_point), kind, fs_type: Some(fs_type), volume }
        })
        .collect();
    extra.sort_by(|a, b| a.path.cmp(&b.path));
    roots.extend(extra);
    roots
}

/// `/proc/mounts` escapes whitespace and backslashes as octal (`\040`).
#[cfg(not(windows))]
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `(total, free)` bytes of the volume holding `path`.
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide = wide(path);
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and the out-params point to valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0 && total > 0).then_some((total, available))
}

/// `(total, free)` bytes of the volume holding `path`.
#[cfg(unix)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain old data and is fully written on success.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    let total = stats.f_blocks as u64 * block;
    (total > 0).then_some((total, stats.f_bavail as u64 * block))
}

#[cfg(not(any(windows, unix)))]
pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

// ======================================================================== paths

pub fn names_equal(a: &OsStr, b: &OsStr) -> bool {
    if cfg!(windows) { a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() } else { a == b }
}

/// Component-wise prefix test, case-insensitive on Windows.
pub fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let mut components = path.components();
    prefix
        .components()
        .all(|want| components.next().is_some_and(|have| names_equal(have.as_os_str(), want.as_os_str())))
}

pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('$') || name == "System Volume Information"
}

/// Turn user input into an absolute, normalised path. Handles `~`, quotes and,
/// on WSL/Linux, Windows-style `C:\...` paths (mapped to `/mnt/c/...`).
pub fn resolve_input_path(input: &str) -> PathBuf {
    let trimmed = input.trim().trim_matches('"');
    let path = if let Some(rest) = trimmed.strip_prefix('~')
        && let Some(home) = home_dir()
    {
        home.join(rest.trim_start_matches(['/', '\\']))
    } else if cfg!(not(windows))
        && let [drive, b':', rest @ ..] = trimmed.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        let rest = String::from_utf8_lossy(rest).replace('\\', "/");
        PathBuf::from(format!("/mnt/{}/{}", drive.to_ascii_lowercase() as char, rest.trim_start_matches('/')))
    } else {
        PathBuf::from(trimmed)
    };
    let absolute = std::path::absolute(&path).unwrap_or(path);
    let mut normalised = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalised.pop();
            }
            other => normalised.push(other),
        }
    }
    normalised
}

/// Shell-style Tab completion of the last path segment against existing directories.
pub fn complete_path(input: &str) -> Option<String> {
    let split = input.rfind(['/', '\\'])? + 1;
    let (dir_part, prefix) = input.split_at(split);
    let listing = list_dir(&resolve_input_path(dir_part)).ok()?;
    let prefix_lower = prefix.to_lowercase();
    let mut matches: Vec<String> =
        listing.dirs.into_iter().map(|(name, _)| name).filter(|n| n.to_lowercase().starts_with(&prefix_lower)).collect();
    matches.sort();
    let first = matches.first()?;
    let common = matches.iter().skip(1).fold(first.len(), |len, other| {
        first
            .char_indices()
            .zip(other.chars())
            .take_while(|((_, a), b)| a.to_lowercase().eq(b.to_lowercase()))
            .map(|((i, a), _)| i + a.len_utf8())
            .last()
            .unwrap_or(0)
            .min(len)
    });
    let mut completed = format!("{dir_part}{}", &first[..common]);
    if matches.len() == 1 {
        completed.push(std::path::MAIN_SEPARATOR);
    }
    Some(completed)
}

// ======================================================================== detection

/// Directory entries split into real subdirectories (symlinks/junctions excluded) and file names.
struct DirListing {
    dirs: Vec<(String, PathBuf)>,
    files: Vec<String>,
}

fn list_dir(path: &Path) -> io::Result<DirListing> {
    let mut listing = DirListing { dirs: Vec::new(), files: Vec::new() };
    for entry in fs::read_dir(path)?.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            listing.dirs.push((name, entry.path()));
        } else if file_type.is_file() {
            listing.files.push(name);
        }
    }
    Ok(listing)
}

const PYTHON_MANIFESTS: [&str; 8] =
    ["pyproject.toml", "setup.py", "setup.cfg", "Pipfile", "poetry.lock", "uv.lock", "tox.ini", "environment.yml"];
const DOTNET_MANIFESTS: [&str; 3] = [".csproj", ".fsproj", ".vbproj"];
const PYTHON_CACHES: [&str; 5] = ["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox"];
const NODE_CACHES: [&str; 6] = [".next", ".nuxt", ".svelte-kit", ".turbo", ".parcel-cache", ".angular"];
const RUST_TARGET_MARKERS: [&str; 4] = ["CACHEDIR.TAG", ".rustc_info.json", "debug", "release"];

/// Languages whose project manifest sits directly in a directory with these files.
fn project_languages(files: &[String]) -> Vec<Language> {
    let lower: Vec<String> = files.iter().map(|f| f.to_lowercase()).collect();
    let mut languages = Vec::new();
    if files.iter().any(|f| f == "Cargo.toml") {
        languages.push(Language::Rust);
    }
    if lower.iter().any(|f| DOTNET_MANIFESTS.iter().any(|ext| f.ends_with(ext))) {
        languages.push(Language::DotNet);
    }
    let requirements = lower.iter().any(|f| f.starts_with("requirements") && f.ends_with(".txt"));
    if requirements || files.iter().any(|f| PYTHON_MANIFESTS.contains(&f.as_str())) {
        languages.push(Language::Python);
    }
    if files.iter().any(|f| f == "package.json") {
        languages.push(Language::Node);
    }
    languages
}

fn has_python_sources(files: &[String]) -> bool {
    files.iter().any(|f| f.ends_with(".py"))
}

/// Decide whether `path` (named `name`, inside a directory with `parent_languages`)
/// is build output. Every rule requires a matching project next to the relic.
fn classify_relic(
    parent_languages: &[Language],
    parent_has_python: bool,
    python_scope: bool,
    name: &str,
    path: &Path,
) -> Option<(Language, &'static str)> {
    if parent_languages.contains(&Language::Rust)
        && name == "target"
        && RUST_TARGET_MARKERS.iter().any(|m| path.join(m).exists())
    {
        return Some((Language::Rust, "target"));
    }
    if parent_languages.contains(&Language::DotNet) {
        if name.eq_ignore_ascii_case("bin") {
            return Some((Language::DotNet, "bin"));
        }
        if name.eq_ignore_ascii_case("obj") {
            return Some((Language::DotNet, "obj"));
        }
    }
    let pythonish = parent_has_python || parent_languages.contains(&Language::Python);
    if pythonish && path.join("pyvenv.cfg").is_file() {
        return Some((Language::Python, "venv"));
    }
    if (pythonish || python_scope) && PYTHON_CACHES.contains(&name) {
        return Some((Language::Python, "cache"));
    }
    if parent_languages.contains(&Language::Node) {
        if name == "node_modules" {
            return Some((Language::Node, "node_modules"));
        }
        if NODE_CACHES.contains(&name) {
            return Some((Language::Node, "cache"));
        }
    }
    None
}

pub fn root_node(root: Root) -> TreeNode {
    let languages = list_dir(&root.path).map(|l| project_languages(&l.files)).unwrap_or_default();
    TreeNode {
        path: root.path.clone(),
        name: root.label.clone(),
        depth: 0,
        expanded: false,
        leaf: false,
        hidden: false,
        python_scope: languages.contains(&Language::Python),
        languages,
        relic: None,
        root: Some(root),
    }
}

pub fn load_children(parent: &TreeNode, show_hidden: bool) -> Vec<TreeNode> {
    let Ok(listing) = list_dir(&parent.path) else { return Vec::new() };
    let languages = project_languages(&listing.files);
    let has_python = has_python_sources(&listing.files);
    let mut dirs = listing.dirs;
    dirs.sort_by_cached_key(|(name, _)| name.to_lowercase());
    dirs.into_iter()
        .filter_map(|(name, path)| {
            let hidden = is_hidden(&name);
            if hidden && !show_hidden {
                return None;
            }
            let relic = classify_relic(&languages, has_python, parent.python_scope, &name, &path).map(|(l, _)| l);
            let (child_languages, leaf) = match list_dir(&path) {
                Ok(child) => (project_languages(&child.files), child.dirs.is_empty()),
                Err(_) => (Vec::new(), true),
            };
            Some(TreeNode {
                depth: parent.depth + 1,
                expanded: false,
                leaf,
                hidden,
                root: None,
                python_scope: parent.python_scope || child_languages.contains(&Language::Python),
                languages: child_languages,
                relic,
                name,
                path,
            })
        })
        .collect()
}

// ======================================================================== scanning

const SKIP_ALWAYS: [&str; 16] = [
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".cargo",
    ".rustup",
    ".nuget",
    ".npm",
    ".pnpm-store",
    ".gradle",
    ".m2",
    "AppData",
    "site-packages",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".Trash",
];
const SKIP_AT_VOLUME_ROOT: [&str; 20] = [
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "proc",
    "sys",
    "dev",
    "run",
    "snap",
    "boot",
    "usr",
    "lib",
    "lib32",
    "lib64",
    "libx32",
    "bin",
    "sbin",
    "etc",
    "var",
    "lost+found",
];

pub fn spawn_scan(root: PathBuf, volume_roots: Vec<PathBuf>, cancel: Arc<AtomicBool>, tx: Sender<ScanEvent>) {
    thread::spawn(move || {
        let (dirs, cancelled) = scan(&root, &volume_roots, &cancel, &tx);
        let _ = tx.send(ScanEvent::Finished { dirs, cancelled });
    });
}

/// Depth-first walk that never descends into relics, VCS metadata or package caches.
fn scan(root: &Path, volume_roots: &[PathBuf], cancel: &AtomicBool, tx: &Sender<ScanEvent>) -> (u64, bool) {
    let emit_relic = |path: &Path, project: &Path, language: Language, kind: &'static str| {
        let _ = tx.send(ScanEvent::Progress { dirs: 0, current: path.to_path_buf() });
        let (bytes, files) = measure(path, cancel);
        let relic = Relic {
            path: path.to_path_buf(),
            project: project.to_path_buf(),
            language,
            kind,
            bytes,
            files,
            selected: true,
            error: None,
        };
        let _ = tx.send(ScanEvent::Relic(relic));
    };

    // The selected folder may itself be a relic (e.g. the user picked a `target/`).
    if let Some(parent) = root.parent()
        && let Some(name) = root.file_name().map(|n| n.to_string_lossy())
        && let Ok(listing) = list_dir(parent)
    {
        let languages = project_languages(&listing.files);
        let has_python = has_python_sources(&listing.files);
        let scope = languages.contains(&Language::Python);
        if let Some((language, kind)) = classify_relic(&languages, has_python, scope, &name, root) {
            emit_relic(root, parent, language, kind);
            return (1, cancel.load(Ordering::Relaxed));
        }
    }

    // (directory, inside a Python project, nearest ancestor project)
    let mut stack: Vec<(PathBuf, bool, Option<PathBuf>)> = vec![(root.to_path_buf(), false, None)];
    let mut dirs = 0u64;
    let mut last_progress = Instant::now();
    while let Some((dir, inherited_scope, inherited_owner)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return (dirs, true);
        }
        dirs += 1;
        if last_progress.elapsed() >= Duration::from_millis(50) {
            let _ = tx.send(ScanEvent::Progress { dirs, current: dir.clone() });
            last_progress = Instant::now();
        }
        let Ok(listing) = list_dir(&dir) else { continue };
        // A virtualenv that does not belong to a project: leave it alone, don't descend.
        if listing.files.iter().any(|f| f == "pyvenv.cfg") {
            continue;
        }
        let languages = project_languages(&listing.files);
        let has_python = has_python_sources(&listing.files);
        let scope = inherited_scope || languages.contains(&Language::Python);
        let owner = if languages.is_empty() {
            inherited_owner
        } else {
            let _ = tx.send(ScanEvent::Project { path: dir.clone(), languages: languages.clone() });
            Some(dir.clone())
        };
        let at_volume_root = dir.parent().is_none() || volume_roots.iter().any(|r| r == &dir);
        for (name, path) in listing.dirs {
            if let Some((language, kind)) = classify_relic(&languages, has_python, scope, &name, &path) {
                emit_relic(&path, owner.as_deref().unwrap_or(&dir), language, kind);
            } else if !(SKIP_ALWAYS.contains(&name.as_str())
                || at_volume_root && SKIP_AT_VOLUME_ROOT.contains(&name.as_str()))
            {
                stack.push((path, scope, owner.clone()));
            }
        }
    }
    (dirs, false)
}

/// `(bytes, files)` below `path`, without following links.
fn measure(path: &Path, cancel: &AtomicBool) -> (u64, u64) {
    let (mut bytes, mut files) = (0, 0);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(_) => {
                    bytes += entry.metadata().map_or(0, |m| m.len());
                    files += 1;
                }
                Err(_) => {}
            }
        }
    }
    (bytes, files)
}

// ======================================================================== cleaning

pub fn spawn_clean(targets: Vec<(PathBuf, u64)>, cancel: Arc<AtomicBool>, tx: Sender<CleanEvent>) {
    thread::spawn(move || {
        for (path, bytes) in targets {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(CleanEvent::Finished { cancelled: true });
                return;
            }
            let _ = tx.send(CleanEvent::Working(path.clone()));
            let event = match remove_relic(&path) {
                Ok(()) => CleanEvent::Removed { path, bytes },
                Err(e) => CleanEvent::Failed { path, bytes, error: e.to_string() },
            };
            let _ = tx.send(event);
        }
        let _ = tx.send(CleanEvent::Finished { cancelled: false });
    });
}

fn remove_relic(path: &Path) -> io::Result<()> {
    // Re-check right before deleting: must still be a real directory, never a link.
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Err(io::Error::other("no longer a directory, skipped"));
    }
    match fs::remove_dir_all(path) {
        Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => {
            clear_readonly(path);
            fs::remove_dir_all(path)
        }
        result => result,
    }
}

/// Windows refuses to delete read-only files; build tools occasionally produce them.
#[cfg(windows)]
fn clear_readonly(path: &Path) {
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() && !meta.file_type().is_symlink() {
                stack.push(entry.path());
            }
            let mut permissions = meta.permissions();
            if permissions.readonly() {
                // Windows-only: clears FILE_ATTRIBUTE_READONLY, no Unix mode bits involved.
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(entry.path(), permissions);
            }
        }
    }
}

#[cfg(not(windows))]
fn clear_readonly(_path: &Path) {}

// ======================================================================== formatting

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

pub fn human_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn human_duration(d: Duration) -> String {
    match d.as_secs() {
        0..60 => format!("{:.1}s", d.as_secs_f64()),
        s => format!("{}m{:02}s", s / 60, s % 60),
    }
}

// ======================================================================== drawing

const ACCENT: Color = Color::Rgb(110, 231, 183);
const DANGER: Color = Color::Rgb(248, 113, 113);
const WARN: Color = Color::Rgb(251, 191, 36);
const MUTED: Color = Color::Rgb(120, 125, 140);
const FAINT: Color = Color::Rgb(60, 64, 76);
const SELECTION: Color = Color::Rgb(38, 44, 58);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn chip(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD))
}

fn panel(title: &str, focused: bool) -> Block<'static> {
    let color = if focused { ACCENT } else { FAINT };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().fg(if focused { ACCENT } else { MUTED }).bold()))
}

/// A thin horizontal meter, `ratio` of `width` filled.
fn meter(width: u16, ratio: f64, color: Color) -> Vec<Span<'static>> {
    let filled = ((width as f64) * ratio.clamp(0.0, 1.0)).round() as usize;
    let empty = (width as usize).saturating_sub(filled);
    vec![Span::styled("━".repeat(filled), Style::new().fg(color)), Span::styled("━".repeat(empty), Style::new().fg(FAINT))]
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height }
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.tick % SPINNER.len()]
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let [left, right] = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body);
    let [location, overview, breakdown, relics] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Fill(1),
    ])
    .areas(right);

    draw_header(frame, header, app);
    draw_tree(frame, left, app);
    draw_location(frame, location, app);
    draw_overview(frame, overview, app);
    draw_breakdown(frame, breakdown, app);
    draw_relics(frame, relics, app);
    draw_footer(frame, footer, app);

    match &app.mode {
        Mode::Confirm => draw_confirm(frame, app),
        Mode::GoTo(input) => draw_goto(frame, input),
        Mode::Help => draw_help(frame),
        Mode::Normal => {}
    }
    if app.clean.is_some() {
        draw_cleaning(frame, app);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let status = if app.scan.is_some() {
        let dirs = app.report.as_ref().map_or(0, |r| r.dirs_scanned);
        Line::from(vec![
            Span::styled(format!("{} scanning ", spinner(app)), Style::new().fg(WARN)),
            Span::styled(format!("{} dirs ", human_count(dirs)), Style::new().fg(MUTED)),
        ])
    } else if app.clean.is_some() {
        Line::from(Span::styled(format!("{} cleaning ", spinner(app)), Style::new().fg(DANGER)))
    } else {
        Line::from(Span::styled("? help  q quit ", Style::new().fg(MUTED)))
    };
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(status.width() as u16)]).areas(area);

    let path = app.tree.selected().map(|n| n.path.display().to_string()).unwrap_or_default();
    let title = Line::from(vec![
        Span::styled(" ▞ devclean ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw(" "),
        chip(&app.platform.label(), app.platform.color()),
        Span::raw("  "),
        Span::styled(path, Style::new().fg(Color::White)),
    ]);
    frame.render_widget(Paragraph::new(title), left);
    frame.render_widget(Paragraph::new(status), right);
}

fn draw_tree(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Tree && app.mode == Mode::Normal;
    let hidden = if app.tree.show_hidden { " .hidden shown " } else { " .hidden off " };
    let block = panel("Explorer", focused).title(Line::from(Span::styled(hidden, Style::new().fg(MUTED))).right_aligned());

    let tree = &mut app.tree;
    let items: Vec<ListItem> = tree
        .nodes
        .iter()
        .map(|node| {
            let mut spans = vec![Span::raw("  ".repeat(node.depth))];
            let arrow = match (node.leaf, node.expanded) {
                (true, _) => "  ",
                (false, true) => "▾ ",
                (false, false) => "▸ ",
            };
            spans.push(Span::styled(arrow, Style::new().fg(MUTED)));
            if let Some(root) = &node.root {
                spans.push(Span::styled(format!("{} ", root.kind.icon()), Style::new().fg(root.kind.color())));
                spans.push(Span::styled(node.name.clone(), Style::new().fg(Color::White).bold()));
                if let Some(volume) = root.volume.as_ref().filter(|v| **v != node.name) {
                    spans.push(Span::styled(format!("  {volume}"), Style::new().fg(MUTED)));
                }
                if root.kind == RootKind::Home {
                    spans.push(Span::styled(format!("  {}", node.path.display()), Style::new().fg(FAINT)));
                }
            } else {
                let style = match (node.relic, node.hidden, node.languages.is_empty()) {
                    (Some(_), _, _) => Style::new().fg(DANGER),
                    (None, true, _) => Style::new().fg(MUTED),
                    (None, false, false) => Style::new().fg(Color::White).bold(),
                    (None, false, true) => Style::new().fg(Color::Gray),
                };
                spans.push(Span::styled(node.name.clone(), style));
            }
            for language in &node.languages {
                spans.push(Span::raw(" "));
                spans.push(chip(language.tag(), language.color()));
            }
            if node.relic.is_some() {
                spans.push(Span::styled("  ✖ relic", Style::new().fg(DANGER).italic()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(SELECTION))
        .highlight_symbol(Span::styled("▌", Style::new().fg(ACCENT)))
        .scroll_padding(3);
    frame.render_stateful_widget(list, area, &mut tree.state);
}

fn draw_location(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Location", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(node) = app.tree.selected() else { return };

    let label = |text: &str| Span::styled(format!("{text:<8}"), Style::new().fg(MUTED));
    let mut lines = vec![Line::from(Span::styled(node.path.display().to_string(), Style::new().fg(Color::White).bold()))];

    if let Some(root) = app.tree.volume_for(&node.path) {
        let mut spans = vec![
            label("Volume"),
            Span::styled(format!("{} ", root.kind.icon()), Style::new().fg(root.kind.color())),
            Span::styled(root.kind.name(), Style::new().fg(root.kind.color())),
            Span::styled(format!("  {}", root.path.display()), Style::new().fg(Color::Gray)),
        ];
        for extra in [&root.volume, &root.fs_type].into_iter().flatten() {
            spans.push(Span::styled(format!(" · {extra}"), Style::new().fg(MUTED)));
        }
        lines.push(Line::from(spans));

        if let Some(Some((total, free))) = app.disk.get(&root.path) {
            let used = 1.0 - *free as f64 / *total as f64;
            let color = if used > 0.9 {
                DANGER
            } else if used > 0.75 {
                WARN
            } else {
                ACCENT
            };
            let text = format!("  {} free of {}  ({:.0}% used)", human_bytes(*free), human_bytes(*total), used * 100.0);
            let width = inner.width.saturating_sub(8 + text.chars().count() as u16).clamp(6, 40);
            let mut spans = vec![label("Disk")];
            spans.extend(meter(width, used, color));
            spans.push(Span::styled(text, Style::new().fg(Color::Gray)));
            lines.push(Line::from(spans));
        }
    }

    let mut status = vec![label("Status")];
    if let Some(language) = node.relic {
        status.push(chip("relic", DANGER));
        status.push(Span::styled(
            format!(" build output of a {} project — clean will remove it", language.name()),
            Style::new().fg(DANGER),
        ));
    } else if !node.languages.is_empty() {
        for language in &node.languages {
            status.push(chip(language.tag(), language.color()));
            status.push(Span::raw(" "));
        }
        let names: Vec<&str> = node.languages.iter().map(|l| l.name()).collect();
        status.push(Span::styled(format!("{} project", names.join(" + ")), Style::new().fg(Color::White)));
    } else if node.python_scope {
        status.push(Span::styled("inside a Python project", Style::new().fg(Language::Python.color())));
    } else {
        status.push(Span::styled("folder — scanning looks for projects below it", Style::new().fg(MUTED)));
    }
    lines.push(Line::from(status));

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
    let report = app.report.as_ref();
    let value = |f: &dyn Fn(&ScanReport) -> String| report.map_or_else(|| "—".to_string(), f);
    let tiles = [
        ("PROJECTS", value(&|r| human_count(r.project_count(None) as u64)), "found".to_string(), Color::White),
        (
            "DIRTY",
            value(&|r| human_count(r.dirty_count(None) as u64)),
            "with build output".to_string(),
            WARN,
        ),
        (
            "RELICS",
            value(&|r| human_count(r.relics.len() as u64)),
            report.map_or(String::new(), |r| format!("{} files", human_count(r.files()))),
            DANGER,
        ),
        (
            "RECLAIMABLE",
            value(&|r| human_bytes(r.selected_bytes())),
            report.map_or(String::new(), |r| format!("of {} total", human_bytes(r.bytes(None)))),
            ACCENT,
        ),
    ];
    let areas = Layout::horizontal([Constraint::Fill(1); 4]).split(area);
    for ((title, big, small, color), tile) in tiles.into_iter().zip(areas.iter()) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(FAINT))
            .title(Span::styled(format!(" {title} "), Style::new().fg(MUTED)));
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(big, Style::new().fg(color).bold())),
            Line::from(Span::styled(small, Style::new().fg(MUTED))),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), *tile);
    }
}

fn draw_breakdown(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Breakdown", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(report) = &app.report else {
        let hint = vec![
            Line::from(""),
            Line::from(Span::styled("Rust target/  ·  .NET bin/ obj/  ·  Python venvs & caches  ·  JS node_modules", Style::new().fg(MUTED))),
            Line::from(""),
            Line::from(vec![
                Span::styled("Pick a folder, then ", Style::new().fg(MUTED)),
                chip("s", ACCENT),
                Span::styled(" scan  or  ", Style::new().fg(MUTED)),
                chip("c", DANGER),
                Span::styled(" clean", Style::new().fg(MUTED)),
            ]),
        ];
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), inner);
        return;
    };

    let total = report.bytes(None);
    let mut lines = Vec::new();
    let fixed = 4 + 12 + 9 + 11 + 11;
    let bar_width = inner.width.saturating_sub(fixed).max(4);
    for language in Language::ALL {
        let bytes = report.bytes(Some(language));
        let ratio = if total == 0 { 0.0 } else { bytes as f64 / total as f64 };
        let mut spans = vec![
            chip(language.tag(), language.color()),
            Span::styled(format!(" {:<11}", language.name()), Style::new().fg(language.color())),
            Span::styled(format!("{:>4} proj", report.project_count(Some(language))), Style::new().fg(Color::Gray)),
            Span::styled(format!("{:>4} dirty ", report.dirty_count(Some(language))), Style::new().fg(MUTED)),
        ];
        spans.extend(meter(bar_width, ratio, language.color()));
        spans.push(Span::styled(format!(" {:>10}", human_bytes(bytes)), Style::new().fg(Color::White).bold()));
        lines.push(Line::from(spans));
    }

    // Stacked composition bar, one segment per language.
    let width = inner.width as usize;
    let mut composition = Vec::new();
    if total == 0 {
        composition.push(Span::styled("▀".repeat(width), Style::new().fg(FAINT)));
    } else {
        let mut used = 0;
        let present: Vec<(Language, u64)> =
            Language::ALL.iter().map(|&l| (l, report.bytes(Some(l)))).filter(|&(_, b)| b > 0).collect();
        for (i, (language, bytes)) in present.iter().enumerate() {
            let cells = if i + 1 == present.len() {
                width - used
            } else {
                ((*bytes as f64 / total as f64) * width as f64).round() as usize
            };
            let cells = cells.min(width - used);
            used += cells;
            composition.push(Span::styled("▀".repeat(cells), Style::new().fg(language.color())));
        }
    }
    lines.push(Line::from(composition));

    let mut kinds = Vec::new();
    for (i, (kind, bytes, count)) in report.kind_breakdown().into_iter().enumerate() {
        if i > 0 {
            kinds.push(Span::styled("  ·  ", Style::new().fg(FAINT)));
        }
        kinds.push(Span::styled(format!("{kind} "), Style::new().fg(Color::White)));
        kinds.push(Span::styled(format!("{count}× {}", human_bytes(bytes)), Style::new().fg(MUTED)));
    }
    lines.push(Line::from(kinds));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_relics(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Relics && app.mode == Mode::Normal;
    let mut block = panel("Relics", focused);
    let Some(report) = &app.report else {
        frame.render_widget(block, area);
        return;
    };

    let title = format!(
        " {} found · {} marked · {} ",
        report.relics.len(),
        report.selected_count(),
        human_bytes(report.selected_bytes())
    );
    block = block.title(Line::from(Span::styled(title, Style::new().fg(MUTED))).right_aligned());
    if let Some(job) = &app.scan {
        let current = job.current.display().to_string();
        block = block.title_bottom(Span::styled(format!(" {} {current} ", spinner(app)), Style::new().fg(WARN)));
    } else {
        block = block.title_bottom(Span::styled(
            format!(" {} in {} ", report.root.display(), human_duration(report.elapsed())),
            Style::new().fg(FAINT),
        ));
    }

    if report.relics.is_empty() {
        let message = if report.running() {
            Span::styled("searching for build output…", Style::new().fg(MUTED))
        } else {
            Span::styled("✨ Spotless. No build relics below this folder.", Style::new().fg(ACCENT).bold())
        };
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let middle = Rect { y: inner.y + inner.height / 2, height: 1.min(inner.height), ..inner };
        frame.render_widget(Paragraph::new(Line::from(message)).alignment(Alignment::Center), middle);
        return;
    }

    let header = Row::new([
        Cell::from(""),
        Cell::from(""),
        Cell::from(Line::from("size").right_aligned()),
        Cell::from(Line::from("files").right_aligned()),
        Cell::from("location"),
    ])
    .style(Style::new().fg(MUTED).bold());
    let rows: Vec<Row> = report
        .relics
        .iter()
        .map(|relic| {
            let check = if relic.selected {
                Span::styled("■", Style::new().fg(ACCENT))
            } else {
                Span::styled("□", Style::new().fg(MUTED))
            };
            let relative = relic.path.strip_prefix(&report.root).unwrap_or(&relic.path);
            let shown = if relative.as_os_str().is_empty() { relic.path.as_path() } else { relative };
            let mut location = vec![Span::styled(
                shown.display().to_string(),
                if relic.selected { Style::new().fg(Color::White) } else { Style::new().fg(MUTED) },
            )];
            if let Some(error) = &relic.error {
                location.push(Span::styled(format!("  ✖ {error}"), Style::new().fg(DANGER)));
            }
            let size_style = if relic.selected { Style::new().fg(Color::White).bold() } else { Style::new().fg(MUTED) };
            Row::new(vec![
                Cell::from(check),
                Cell::from(chip(relic.language.tag(), relic.language.color())),
                Cell::from(Line::from(Span::styled(human_bytes(relic.bytes), size_style)).right_aligned()),
                Cell::from(Line::from(Span::styled(human_count(relic.files), Style::new().fg(MUTED))).right_aligned()),
                Cell::from(Line::from(location)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Length(1), Constraint::Length(4), Constraint::Length(10), Constraint::Length(9), Constraint::Fill(1)],
    )
    .header(header)
    .block(block)
    .column_spacing(1)
    .row_highlight_style(Style::new().bg(if focused { SELECTION } else { Color::Reset }))
    .highlight_symbol(Span::styled(if focused { "▌" } else { " " }, Style::new().fg(ACCENT)));
    frame.render_stateful_widget(table, area, &mut app.relic_state);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hints: &[(&str, &str)] = match (&app.mode, app.focus) {
        _ if app.clean.is_some() => &[("esc", "stop")],
        (Mode::GoTo(_), _) => &[("tab", "complete"), ("enter", "jump"), ("esc", "cancel")],
        (Mode::Confirm, _) => &[("y", "delete"), ("n", "cancel")],
        (Mode::Help, _) => &[("any key", "close")],
        (Mode::Normal, Focus::Tree) => &[
            ("↑↓", "move"),
            ("←→", "fold"),
            ("/", "go to"),
            ("s", "scan"),
            ("c", "clean"),
            (".", "hidden"),
            ("tab", "relics"),
        ],
        (Mode::Normal, Focus::Relics) => {
            &[("↑↓", "move"), ("space", "mark"), ("a", "all"), ("⏎", "show"), ("c", "clean"), ("tab", "tree")]
        }
    };
    let mut spans = vec![Span::raw(" ")];
    if app.scan.is_some() {
        spans.push(Span::styled("esc", Style::new().fg(WARN).bold()));
        spans.push(Span::styled(" cancel scan  ", Style::new().fg(MUTED)));
    }
    for (key, what) in hints {
        spans.push(Span::styled(*key, Style::new().fg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {what}  "), Style::new().fg(MUTED)));
    }
    let hints = Line::from(spans);

    let banner = app.banner.as_ref().filter(|(_, _, at)| at.elapsed() < Duration::from_secs(8)).map(|(text, tone, _)| {
        let color = match tone {
            Tone::Info => Color::Gray,
            Tone::Good => ACCENT,
            Tone::Warn => WARN,
        };
        Line::from(Span::styled(format!("{text} "), Style::new().fg(color).bold()))
    });
    let banner_width = banner.as_ref().map_or(0, |b| b.width() as u16).min(area.width / 2 + 10);
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(banner_width)]).areas(area);
    frame.render_widget(Paragraph::new(hints), left);
    if let Some(banner) = banner {
        frame.render_widget(Paragraph::new(banner).alignment(Alignment::Right), right);
    }
}

fn modal(frame: &mut Frame, width: u16, height: u16, title: &str, color: Color) -> Rect {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().fg(color).bold()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner }
}

fn draw_confirm(frame: &mut Frame, app: &App) {
    let Some(report) = &app.report else { return };
    let by_language = report.selected_by_language();
    let area = modal(frame, 66, 11 + by_language.len() as u16, "Clean build relics?", DANGER);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("in  ", Style::new().fg(MUTED)),
            Span::styled(report.root.display().to_string(), Style::new().fg(Color::White).bold()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!("{} relics across {} projects", report.selected_count(), report.selected_projects()),
            Style::new().fg(Color::Gray),
        )),
    ];
    for (language, count, bytes) in by_language {
        lines.push(Line::from(vec![
            Span::raw("  "),
            chip(language.tag(), language.color()),
            Span::styled(format!(" {:<12}", language.name()), Style::new().fg(language.color())),
            Span::styled(format!("{count:>4} dirs  "), Style::new().fg(MUTED)),
            Span::styled(format!("{:>10}", human_bytes(bytes)), Style::new().fg(Color::White)),
        ]));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(human_bytes(report.selected_bytes()), Style::new().fg(DANGER).bold()),
            Span::styled(" will be permanently deleted (no recycle bin).", Style::new().fg(Color::Gray)),
        ]),
        Line::from(""),
        Line::from(vec![
            chip("y", DANGER),
            Span::styled(" delete      ", Style::new().fg(Color::Gray)),
            chip("n", MUTED),
            Span::styled(" cancel", Style::new().fg(Color::Gray)),
        ])
        .centered(),
    ]);
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_cleaning(frame: &mut Frame, app: &App) {
    let Some(job) = &app.clean else { return };
    let area = modal(frame, 72, 9, &format!("{} Cleaning", spinner(app)), DANGER);
    let [counts, gauge, _, current] = Layout::vertical([Constraint::Length(1); 4]).areas(area);

    let counts_line = Line::from(vec![
        Span::styled(format!("{}/{}", job.done, job.total), Style::new().fg(Color::White).bold()),
        Span::styled(" relics  ·  ", Style::new().fg(MUTED)),
        Span::styled(human_bytes(job.freed), Style::new().fg(ACCENT).bold()),
        Span::styled(" freed", Style::new().fg(MUTED)),
        if job.failed > 0 {
            Span::styled(format!("  ·  {} failed", job.failed), Style::new().fg(DANGER))
        } else {
            Span::raw("")
        },
    ]);
    frame.render_widget(Paragraph::new(counts_line), counts);
    frame.render_widget(
        Gauge::default()
            .ratio(job.ratio())
            .label(format!("{} / {}", human_bytes(job.done_bytes), human_bytes(job.total_bytes)))
            .gauge_style(Style::new().fg(DANGER).bg(FAINT))
            .use_unicode(true),
        gauge,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(job.current.display().to_string(), Style::new().fg(MUTED))),
        current,
    );
}

fn draw_goto(frame: &mut Frame, input: &str) {
    let area = modal(frame, 80, 5, "Go to folder", ACCENT);
    let lines = vec![
        Line::from(vec![
            Span::styled("› ", Style::new().fg(ACCENT).bold()),
            Span::styled(input.to_string(), Style::new().fg(Color::White)),
            Span::styled("█", Style::new().fg(ACCENT).add_modifier(Modifier::SLOW_BLINK)),
        ]),
        Line::from(Span::styled("~ and C:\\ style paths work · tab completes · enter jumps", Style::new().fg(MUTED))),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_help(frame: &mut Frame) {
    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "Explorer",
            &[
                ("↑ ↓  j k", "move"),
                ("→  l  enter", "expand / step in"),
                ("←  h", "collapse / parent"),
                ("backspace", "jump to parent"),
                ("pgup pgdn home end", "page"),
                ("/  g", "go to a path (tab completes)"),
                (".", "toggle hidden folders"),
                ("r", "reload from disk"),
            ],
        ),
        (
            "Scan & clean",
            &[
                ("s", "scan selected folder recursively"),
                ("c", "clean selected folder (scans first)"),
                ("tab", "switch to relic list"),
                ("space", "mark / unmark relic"),
                ("a", "mark all / none"),
                ("enter", "show relic in explorer"),
                ("esc", "cancel scan / stop clean"),
            ],
        ),
        (
            "What counts as a relic",
            &[
                ("target/", "next to Cargo.toml, with cargo markers"),
                ("bin/ obj/", "next to a .csproj / .fsproj / .vbproj"),
                ("venv", "dir with pyvenv.cfg in a Python project"),
                ("caches", "__pycache__ .pytest_cache .mypy_cache …"),
                ("node_modules/", "next to a package.json"),
                ("JS caches", ".next .nuxt .svelte-kit .turbo .parcel-cache …"),
            ],
        ),
    ];
    let height = sections.iter().map(|(_, rows)| rows.len() as u16 + 2).sum::<u16>() + 2;
    let area = modal(frame, 64, height, "Keys", ACCENT);
    let mut lines = Vec::new();
    for (title, rows) in sections {
        lines.push(Line::from(Span::styled(title, Style::new().fg(ACCENT).bold())));
        for (key, what) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<20}"), Style::new().fg(Color::White)),
                Span::styled(*what, Style::new().fg(MUTED)),
            ]));
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn touch(root: &Path, relative: &str, bytes: usize) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("devclean-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // real relics
        touch(&root, "rust_app/Cargo.toml", 10);
        touch(&root, "rust_app/target/CACHEDIR.TAG", 10);
        touch(&root, "rust_app/target/debug/app.exe", 4000);
        touch(&root, "dotnet/Api/Api.csproj", 10);
        touch(&root, "dotnet/Api/bin/Debug/Api.dll", 3000);
        touch(&root, "dotnet/Api/obj/project.assets.json", 500);
        touch(&root, "py_app/pyproject.toml", 10);
        touch(&root, "py_app/.venv/pyvenv.cfg", 10);
        touch(&root, "py_app/.venv/Lib/site-packages/big.py", 2000);
        touch(&root, "py_app/pkg/mod.py", 10);
        touch(&root, "py_app/pkg/__pycache__/mod.cpython-312.pyc", 100);
        touch(&root, "scripts/main.py", 10);
        touch(&root, "scripts/venv/pyvenv.cfg", 10);
        touch(&root, "web/package.json", 10);
        touch(&root, "web/node_modules/react/index.js", 6000);
        touch(&root, "web/node_modules/react/package.json", 10);
        touch(&root, "web/.next/cache/x", 50);
        touch(&root, "web/packages/ui/package.json", 10);
        touch(&root, "web/packages/ui/node_modules/lib.js", 20);
        // look-alikes that must survive
        touch(&root, "no_manifest/target/debug/keep.txt", 10);
        touch(&root, "rust_no_markers/Cargo.toml", 10);
        touch(&root, "rust_no_markers/target/keep.txt", 10);
        touch(&root, "not_dotnet/bin/tool.sh", 10);
        touch(&root, "loose/node_modules/keep.js", 10);
        touch(&root, "stray_env/pyvenv.cfg", 10);
        touch(&root, "stray_env/inner/Cargo.toml", 10);
        touch(&root, "stray_env/inner/target/CACHEDIR.TAG", 10);
        touch(&root, "rust_app/.git/target/CACHEDIR.TAG", 10);
        root
    }

    fn run_scan(root: &Path) -> ScanReport {
        let (tx, rx) = mpsc::channel();
        let (dirs, cancelled) = scan(root, &[], &AtomicBool::new(false), &tx);
        drop(tx);
        let mut report = ScanReport::new(root.to_path_buf());
        report.dirs_scanned = dirs;
        for event in rx {
            match event {
                ScanEvent::Project { path, languages } => report.projects.push((path, languages)),
                ScanEvent::Relic(relic) => report.insert_relic(relic),
                _ => {}
            }
        }
        report.finish(cancelled);
        report
    }

    #[test]
    fn finds_only_real_relics() {
        let root = fixture();
        let report = run_scan(&root);
        let mut found: Vec<String> = report
            .relics
            .iter()
            .map(|r| r.path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                "dotnet/Api/bin",
                "dotnet/Api/obj",
                "py_app/.venv",
                "py_app/pkg/__pycache__",
                "rust_app/target",
                "scripts/venv",
                "web/.next",
                "web/node_modules",
                "web/packages/ui/node_modules"
            ]
        );
        assert_eq!(report.bytes(Some(Language::Rust)), 4010);
        assert_eq!(report.bytes(Some(Language::DotNet)), 3500);
        assert_eq!(report.relics[0].kind, "node_modules");
        // scripts/ has no manifest but owns a venv, so it counts as a project
        assert_eq!(report.project_count(Some(Language::Python)), 2);
        assert_eq!(report.project_count(Some(Language::Node)), 2);
        assert_eq!(report.bytes(Some(Language::Node)), 6080);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn selecting_a_relic_directly_scans_just_that() {
        let root = fixture();
        let report = run_scan(&root.join("rust_app").join("target"));
        assert_eq!(report.relics.len(), 1);
        assert_eq!(report.relics[0].project, root.join("rust_app"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn clean_removes_relics_and_keeps_projects() {
        let root = fixture();
        let report = run_scan(&root);
        let targets = report.relics.iter().map(|r| (r.path.clone(), r.bytes)).collect();
        let (tx, rx) = mpsc::channel();
        spawn_clean(targets, Arc::new(AtomicBool::new(false)), tx);
        let removed = rx.iter().filter(|e| matches!(e, CleanEvent::Removed { .. })).count();
        assert_eq!(removed, 9);
        assert!(!root.join("rust_app/target").exists());
        assert!(root.join("rust_app/Cargo.toml").exists());
        assert!(root.join("py_app/pkg/mod.py").exists());
        assert!(root.join("no_manifest/target/debug/keep.txt").exists());
        assert!(root.join("loose/node_modules/keep.js").exists());
        assert!(root.join("web/package.json").exists());
        assert!(root.join("stray_env/inner/target").exists());
        assert!(run_scan(&root).relics.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn formatting() {
        assert_eq!(human_count(1_234_567), "1,234,567");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(500 * 1024 * 1024 * 1024), "500 GiB");
    }

    #[test]
    fn path_input_is_normalised() {
        let resolved = resolve_input_path("./a/../b");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("b"));
        assert!(!resolved.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)));
    }

    /// `cargo test render -- --nocapture` prints the full UI.
    #[test]
    fn render_snapshot() {
        let root = fixture();
        let mut app = App::new(Some(root.clone()));
        app.report = Some(run_scan(&root));
        app.relic_state.select(Some(0));
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let line: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
            println!("{line}");
        }
        app.mode = Mode::Confirm;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        for y in 8..32 {
            let line: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect();
            println!("{line}");
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
