//! Directory listing, sorting, and file metadata for the picker, following
//! yazi's defaults: directories first, case-insensitive alphabetical order,
//! dot files hidden.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::natsort::natsort;

/// Bytes read from a file for its text preview.
const PREVIEW_BYTES: u64 = 64 * 1024;
/// Lines kept from a text preview.
const PREVIEW_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Fifo,
    Socket,
    Block,
    Char,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Display name; lossy for names that are not valid UTF-8.
    pub name: String,
    pub os_name: OsString,
    pub path: PathBuf,
    /// Type after following symlinks (a broken link stays `Symlink`).
    pub kind: Kind,
    pub is_link: bool,
    /// `readlink` value for symlinks.
    pub link_to: Option<PathBuf>,
    pub len: u64,
    pub mtime: Option<SystemTime>,
    pub btime: Option<SystemTime>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub hidden: bool,
}

impl Entry {
    pub fn from_path(path: &Path) -> Option<Self> {
        let lmeta = std::fs::symlink_metadata(path).ok()?;
        let os_name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| path.as_os_str().to_os_string());
        let is_link = lmeta.file_type().is_symlink();
        let meta = if is_link {
            std::fs::metadata(path).ok()
        } else {
            None
        };
        let m = meta.as_ref().unwrap_or(&lmeta);
        let kind = if is_link && meta.is_none() {
            Kind::Symlink
        } else {
            kind_of(m)
        };
        Some(Self {
            name: os_name.to_string_lossy().into_owned(),
            hidden: is_hidden(&os_name, &lmeta),
            os_name,
            path: path.to_path_buf(),
            kind,
            is_link,
            link_to: is_link.then(|| std::fs::read_link(path).ok()).flatten(),
            len: m.len(),
            mtime: m.modified().ok(),
            btime: m.created().ok(),
            mode: mode_of(m),
            uid: uid_of(m),
            gid: gid_of(m),
        })
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn is_file(&self) -> bool {
        self.kind == Kind::File
    }

    /// A symlink whose target does not exist.
    pub fn is_orphan(&self) -> bool {
        self.is_link && self.kind == Kind::Symlink
    }

    pub fn is_exec(&self) -> bool {
        self.is_file() && self.mode & 0o111 != 0
    }

    pub fn name_bytes(&self) -> Vec<u8> {
        os_bytes(&self.os_name)
    }

    /// Extension of regular files only, as yazi's extension sort uses.
    pub fn ext(&self) -> Option<String> {
        if !self.is_file() {
            return None;
        }
        Path::new(&self.os_name)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
    }
}

#[cfg(unix)]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

fn kind_of(m: &std::fs::Metadata) -> Kind {
    let t = m.file_type();
    if t.is_dir() {
        return Kind::Dir;
    }
    if t.is_file() {
        return Kind::File;
    }
    if t.is_symlink() {
        return Kind::Symlink;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if t.is_fifo() {
            return Kind::Fifo;
        }
        if t.is_socket() {
            return Kind::Socket;
        }
        if t.is_block_device() {
            return Kind::Block;
        }
        if t.is_char_device() {
            return Kind::Char;
        }
    }
    Kind::Other
}

#[cfg(unix)]
fn mode_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode()
}
#[cfg(not(unix))]
fn mode_of(_: &std::fs::Metadata) -> u32 {
    0
}
#[cfg(unix)]
fn uid_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.uid()
}
#[cfg(not(unix))]
fn uid_of(_: &std::fs::Metadata) -> u32 {
    0
}
#[cfg(unix)]
fn gid_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.gid()
}
#[cfg(not(unix))]
fn gid_of(_: &std::fs::Metadata) -> u32 {
    0
}

/// A dot prefix on Unix; the hidden or system attribute on Windows.
#[cfg(not(windows))]
fn is_hidden(name: &std::ffi::OsStr, _: &std::fs::Metadata) -> bool {
    os_bytes(name).first() == Some(&b'.')
}

#[cfg(windows)]
fn is_hidden(_: &std::ffi::OsStr, m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    m.file_attributes() & (HIDDEN | SYSTEM) != 0
}

/// Lists a directory, without sorting or hiding anything.
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    Ok(read
        .filter_map(|e| e.ok())
        .filter_map(|e| Entry::from_path(&e.path()))
        .collect())
}

// ---------- Sorting ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Alphabetical,
    Natural,
    Mtime,
    Btime,
    Extension,
    Size,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sorter {
    pub by: SortBy,
    pub reverse: bool,
    pub dir_first: bool,
    pub sensitive: bool,
}

impl Default for Sorter {
    fn default() -> Self {
        Self {
            by: SortBy::Alphabetical,
            reverse: false,
            dir_first: true,
            sensitive: false,
        }
    }
}

/// ASCII-lowercased byte comparison, then length, as yazi's `cmp_insensitive`.
fn cmp_insensitive(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        match a[i].to_ascii_lowercase().cmp(&b[i].to_ascii_lowercase()) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

impl Sorter {
    /// Sorts in place. `seed` drives the random order.
    pub fn sort(&self, entries: &mut [Entry], seed: &mut u64) {
        if self.by == SortBy::Random {
            if self.dir_first {
                entries.sort_by_key(|e| !e.is_dir());
                let split = entries
                    .iter()
                    .position(|e| !e.is_dir())
                    .unwrap_or(entries.len());
                let (dirs, files) = entries.split_at_mut(split);
                shuffle(dirs, seed);
                shuffle(files, seed);
            } else {
                shuffle(entries, seed);
            }
            return;
        }
        entries.sort_by(|a, b| self.cmp(a, b));
    }

    fn cmp(&self, a: &Entry, b: &Entry) -> Ordering {
        if self.dir_first {
            match b.is_dir().cmp(&a.is_dir()) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        let (x, y) = if self.reverse { (b, a) } else { (a, b) };
        let primary = match self.by {
            SortBy::Alphabetical => {
                let (xb, yb) = (x.name_bytes(), y.name_bytes());
                if self.sensitive {
                    xb.cmp(&yb)
                } else {
                    cmp_insensitive(&xb, &yb)
                }
            }
            SortBy::Natural => natsort(&x.name_bytes(), &y.name_bytes(), !self.sensitive),
            SortBy::Mtime => x.mtime.cmp(&y.mtime),
            SortBy::Btime => x.btime.cmp(&y.btime),
            SortBy::Extension => {
                let (xe, ye) = (x.ext().unwrap_or_default(), y.ext().unwrap_or_default());
                if self.sensitive {
                    xe.cmp(&ye)
                } else {
                    cmp_insensitive(xe.as_bytes(), ye.as_bytes())
                }
            }
            SortBy::Size => x.len.cmp(&y.len),
            SortBy::Random => Ordering::Equal,
        };
        // Ties fall back to a case-sensitive byte comparison that follows `reverse`.
        primary.then_with(|| x.name_bytes().cmp(&y.name_bytes()))
    }
}

/// Fisher-Yates with xorshift; yazi's random comparator is not a total order.
fn shuffle(items: &mut [Entry], seed: &mut u64) {
    for i in (1..items.len()).rev() {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        let j = (*seed % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

// ---------- Formatting ----------

/// yazi's `readable_size`: 1024 steps, one decimal, first ".0" removed.
pub fn readable_size(size: u64) -> String {
    const UNITS: [&str; 11] = ["B", "K", "M", "G", "T", "P", "E", "Z", "Y", "R", "Q"];
    let mut value = size as f64;
    let mut i = 0;
    while value > 1024.0 && i < UNITS.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    format!("{value:.1}{}", UNITS[i]).replacen(".0", "", 1)
}

/// `ls`-style permission string such as `drwxr-xr-x`, as in yazi's status bar.
pub fn permissions(e: &Entry) -> Option<String> {
    if cfg!(not(unix)) {
        return None;
    }
    let m = e.mode;
    let mut s = *b"----------";
    s[0] = match e.kind {
        Kind::Dir => b'd',
        Kind::Symlink => b'l',
        Kind::Block => b'b',
        Kind::Char => b'c',
        Kind::Socket => b's',
        Kind::Fifo => b'p',
        _ => b'-',
    };
    let bit = |mask: u32, c: u8| if m & mask != 0 { c } else { b'-' };
    s[1] = bit(0o400, b'r');
    s[2] = bit(0o200, b'w');
    s[3] = match (m & 0o100 != 0, m & 0o4000 != 0) {
        (true, true) => b's',
        (false, true) => b'S',
        (true, false) => b'x',
        (false, false) => b'-',
    };
    s[4] = bit(0o040, b'r');
    s[5] = bit(0o020, b'w');
    s[6] = match (m & 0o010 != 0, m & 0o2000 != 0) {
        (true, true) => b's',
        (false, true) => b'S',
        (true, false) => b'x',
        (false, false) => b'-',
    };
    s[7] = bit(0o004, b'r');
    s[8] = bit(0o002, b'w');
    s[9] = match (m & 0o001 != 0, m & 0o1000 != 0) {
        (true, true) => b't',
        (false, true) => b'T',
        (true, false) => b'x',
        (false, false) => b'-',
    };
    Some(String::from_utf8_lossy(&s).into_owned())
}

/// Month and day separated as the locale does (`-` for CJK locales).
fn month_day_pattern() -> &'static str {
    let lang = ["LC_ALL", "LC_TIME", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    if ["zh", "ja", "ko"].iter().any(|p| lang.starts_with(p)) {
        "%m-%d"
    } else {
        "%m/%d"
    }
}

/// yazi's mtime/btime linemode: `MM/DD HH:MM` this year, else `MM/DD  YYYY`.
pub fn linemode_time(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return String::new();
    };
    let dt = chrono::DateTime::<chrono::Local>::from(t);
    let md = month_day_pattern();
    use chrono::Datelike;
    if dt.year() == chrono::Local::now().year() {
        dt.format(&format!("{md} %H:%M")).to_string()
    } else {
        dt.format(&format!("{md}  %Y")).to_string()
    }
}

/// Full local time for the spot popup, like strftime `%c` in the C locale.
pub fn full_time(t: Option<SystemTime>) -> String {
    t.map(|t| {
        chrono::DateTime::<chrono::Local>::from(t)
            .format("%a %b %e %H:%M:%S %Y")
            .to_string()
    })
    .unwrap_or_else(|| "-".into())
}

/// `user:group`, falling back to numeric ids.
pub fn owner(e: &Entry) -> String {
    format!(
        "{}:{}",
        lookup_id("/etc/passwd", e.uid).unwrap_or_else(|| e.uid.to_string()),
        lookup_id("/etc/group", e.gid).unwrap_or_else(|| e.gid.to_string())
    )
}

/// Names from `/etc/passwd` or `/etc/group`, read once per process.
fn lookup_id(file: &str, id: u32) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static USERS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    static GROUPS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    let cell = if file.ends_with("passwd") {
        &USERS
    } else {
        &GROUPS
    };
    cell.get_or_init(|| {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut parts = line.split(':');
                let name = parts.next()?;
                let _password = parts.next()?;
                Some((parts.next()?.parse::<u32>().ok()?, name.to_string()))
            })
            .collect()
    })
    .get(&id)
    .cloned()
}

// ---------- Paths ----------

/// Removes `.` and resolves `..` lexically, as yazi's `clean`.
pub fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // `/..` stays `/`; only relative paths keep a leading `..`.
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

pub fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// Expands `$VAR`/`${VAR}` (unset ones stay literal), `~`, and relative paths
/// against `cwd`, then cleans the result.
pub fn expand(raw: &str, cwd: &Path) -> PathBuf {
    let vars = regex::Regex::new(r"\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))")
        .expect("valid regex");
    let expanded = vars.replace_all(raw, |c: &regex::Captures| {
        let name = c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str());
        std::env::var(name).unwrap_or_else(|_| c[0].to_string())
    });
    let s = expanded.as_ref();
    let path = if s == "~" {
        home().unwrap_or_else(|| PathBuf::from("~"))
    } else if let Some(rest) = s.strip_prefix("~/") {
        home().map_or_else(|| PathBuf::from(s), |h| h.join(rest))
    } else {
        let p = PathBuf::from(s);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    clean(&path)
}

/// Replaces a leading home directory with `~`, like yazi's header.
pub fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(h) = home() {
        let h = h.display().to_string();
        if s == h {
            return "~".into();
        }
        if let Some(rest) = s.strip_prefix(&format!("{h}/")) {
            return format!("~/{rest}");
        }
    }
    s
}

// ---------- Preview ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Text(Vec<String>),
    Binary,
    /// Not a regular file (fifo, socket, device): never opened.
    Special(&'static str),
    Unreadable(String),
}

pub fn preview_file(e: &Entry) -> Preview {
    match e.kind {
        Kind::File => {}
        Kind::Fifo => return Preview::Special("named pipe"),
        Kind::Socket => return Preview::Special("socket"),
        Kind::Block => return Preview::Special("block device"),
        Kind::Char => return Preview::Special("character device"),
        Kind::Symlink => return Preview::Special("broken symbolic link"),
        Kind::Dir | Kind::Other => return Preview::Special("special file"),
    }
    // The listing may be stale: check the type again right before opening, so a
    // file replaced by a fifo or device cannot block the UI in `open`.
    match std::fs::metadata(&e.path) {
        Ok(m) if m.is_file() => {}
        Ok(m) => {
            let now = Entry {
                kind: kind_of(&m),
                ..e.clone()
            };
            return match now.kind {
                Kind::File => Preview::Special("special file"),
                _ => preview_file(&now),
            };
        }
        Err(err) => return Preview::Unreadable(err.to_string()),
    }
    let mut buf = Vec::new();
    let read =
        std::fs::File::open(&e.path).and_then(|f| f.take(PREVIEW_BYTES).read_to_end(&mut buf));
    if let Err(err) = read {
        return Preview::Unreadable(err.to_string());
    }
    if buf.contains(&0) {
        return Preview::Binary;
    }
    let text = match std::str::from_utf8(&buf) {
        Ok(t) => t.to_string(),
        // The buffer may end in the middle of a multi-byte character.
        Err(err) if err.valid_up_to() + 4 >= buf.len() => {
            String::from_utf8_lossy(&buf[..err.valid_up_to()]).into_owned()
        }
        Err(_) => return Preview::Binary,
    };
    Preview::Text(
        text.lines()
            .take(PREVIEW_LINES)
            .map(|l| l.replace('\t', "  "))
            .collect(),
    )
}

/// File category used for name colours, from yazi's default theme rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Dir,
    Exec,
    Orphan,
    Image,
    Media,
    Archive,
    Document,
    Plain,
}

pub fn category(e: &Entry) -> Category {
    if e.is_orphan() {
        return Category::Orphan;
    }
    if e.is_dir() {
        return Category::Dir;
    }
    let ext = e.ext().map(|x| x.to_ascii_lowercase()).unwrap_or_default();
    let is = |list: &[&str]| list.contains(&ext.as_str());
    if is(&[
        "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "tif", "tiff", "ico", "heic", "avif",
    ]) {
        Category::Image
    } else if is(&[
        "mp3", "mp4", "mkv", "avi", "mov", "flac", "wav", "ogg", "webm", "m4a", "aac", "wmv",
    ]) {
        Category::Media
    } else if is(&[
        "zip", "rar", "7z", "tar", "gz", "tgz", "xz", "zst", "bz2", "lzma", "cpio", "cab",
    ]) {
        Category::Archive
    } else if is(&["pdf", "doc", "rtf"]) {
        Category::Document
    } else if e.is_exec() {
        Category::Exec
    } else {
        Category::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            os_name: name.into(),
            path: PathBuf::from("/x").join(name),
            kind,
            is_link: false,
            link_to: None,
            len: 0,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 0,
            gid: 0,
            hidden: name.starts_with('.'),
        }
    }

    fn names(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn default_sort_is_dirs_first_case_insensitive() {
        let mut v = vec![
            entry("b.txt", Kind::File),
            entry("A.txt", Kind::File),
            entry("zdir", Kind::Dir),
            entry("file10", Kind::File),
            entry("file2", Kind::File),
            entry("作业.pdf", Kind::File),
        ];
        Sorter::default().sort(&mut v, &mut 1);
        assert_eq!(
            names(&v),
            ["zdir", "A.txt", "b.txt", "file10", "file2", "作业.pdf"]
        );
        let natural = Sorter {
            by: SortBy::Natural,
            ..Sorter::default()
        };
        natural.sort(&mut v, &mut 1);
        assert_eq!(names(&v)[3..5], ["file2", "file10"]);
        let reversed = Sorter {
            reverse: true,
            ..Sorter::default()
        };
        reversed.sort(&mut v, &mut 1);
        assert_eq!(names(&v)[0], "zdir"); // directories stay first
        assert_eq!(names(&v)[1], "作业.pdf");
    }

    #[test]
    fn extension_sort_puts_extensionless_first() {
        let mut v = vec![
            entry("b.pdf", Kind::File),
            entry("a.md", Kind::File),
            entry("Makefile", Kind::File),
        ];
        Sorter {
            by: SortBy::Extension,
            ..Sorter::default()
        }
        .sort(&mut v, &mut 1);
        assert_eq!(names(&v), ["Makefile", "a.md", "b.pdf"]);
    }

    #[test]
    fn random_sort_keeps_dirs_first() {
        let mut v: Vec<Entry> = (0..20)
            .map(|i| {
                entry(
                    &format!("f{i}"),
                    if i % 2 == 0 { Kind::Dir } else { Kind::File },
                )
            })
            .collect();
        Sorter {
            by: SortBy::Random,
            ..Sorter::default()
        }
        .sort(&mut v, &mut 0x9e3779b97f4a7c15);
        assert!(v[..10].iter().all(Entry::is_dir));
        assert!(v[10..].iter().all(|e| !e.is_dir()));
    }

    #[test]
    fn readable_size_matches_yazi() {
        assert_eq!(readable_size(0), "0B");
        assert_eq!(readable_size(1000), "1000B");
        assert_eq!(readable_size(1024), "1024B");
        assert_eq!(readable_size(1536), "1.5K");
        assert_eq!(readable_size(10 * 1024 + 1), "10K");
    }

    #[test]
    fn permission_strings_cover_special_bits() {
        let mut e = entry("d", Kind::Dir);
        e.mode = 0o1777;
        if cfg!(unix) {
            assert_eq!(permissions(&e).unwrap(), "drwxrwxrwt");
            e.kind = Kind::File;
            e.mode = 0o4644;
            assert_eq!(permissions(&e).unwrap(), "-rwSr--r--");
        }
    }

    #[test]
    fn expand_handles_home_vars_and_dots() {
        let cwd = Path::new("/srv/course/hw");
        assert_eq!(
            expand("../slides/./a.pdf", cwd),
            PathBuf::from("/srv/course/slides/a.pdf")
        );
        assert_eq!(
            expand("/tmp/$THU_LEARN_UNSET_VAR/x", cwd),
            PathBuf::from("/tmp/$THU_LEARN_UNSET_VAR/x")
        );
        if let Some(h) = home() {
            assert_eq!(expand("~", cwd), h);
            assert_eq!(expand("~/Downloads", cwd), h.join("Downloads"));
        }
    }

    #[test]
    fn clean_keeps_the_root() {
        assert_eq!(clean(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(clean(Path::new("/a/../../b")), PathBuf::from("/b"));
        assert_eq!(clean(Path::new("../x")), PathBuf::from("../x"));
    }

    #[cfg(unix)]
    #[test]
    fn stale_listing_does_not_open_a_new_fifo() {
        let dir = std::env::temp_dir().join(format!("thu-learn-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x");
        std::fs::write(&path, b"text").unwrap();
        let listed = Entry::from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let made = std::process::Command::new("mkfifo").arg(&path).status();
        if made.is_ok_and(|s| s.success()) {
            assert_eq!(preview_file(&listed), Preview::Special("named pipe"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn special_files_are_never_opened_for_preview() {
        let fifo = entry("pipe", Kind::Fifo);
        assert_eq!(preview_file(&fifo), Preview::Special("named pipe"));
        let dev = entry("tty", Kind::Char);
        assert_eq!(preview_file(&dev), Preview::Special("character device"));
    }
}
