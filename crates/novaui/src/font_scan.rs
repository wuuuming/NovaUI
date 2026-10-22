//! The fonts installed on the system, by family (`system_fonts`), and the faces of one family
//! for `Tree::set_font` and the default fallback chain (`text`). Each file is read only as far as its table directory, `name` and `OS/2`:
//! a few kilobytes of a font folder of hundreds of megabytes, once per process.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An installed font family.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemFont {
    /// Its name as `set_font` takes it (the English name, else the first the font gives).
    pub family: String,
    /// Its name by Windows language id (0x0409 English, 0x0804 Simplified Chinese, …).
    pub names: Vec<(u16, String)>,
}

impl SystemFont {
    /// Its name in the first of `langs` it has one in, else `family`.
    pub fn name(&self, langs: &[u16]) -> &str {
        langs
            .iter()
            .find_map(|l| self.names.iter().find(|(n, _)| n == l).map(|(_, s)| s.as_str()))
            .unwrap_or(&self.family)
    }
}

/// One face in one file (a collection holds several).
pub(crate) struct ScannedFace {
    pub path: PathBuf,
    pub index: u32,
    pub family: String,
    pub names: Vec<(u16, String)>,
    pub weight: u16,
    pub italic: bool,
    /// Its width class is normal (a condensed face can share its family's name).
    pub normal_width: bool,
    /// Has a `wght` axis: one file, every weight.
    pub variable: bool,
}

/// Every installed family, by `family`, once per process.
pub fn system_fonts() -> Vec<SystemFont> {
    let mut out: Vec<SystemFont> = Vec::new();
    for f in scanned() {
        match out.iter_mut().find(|x| x.family == f.family) {
            Some(x) => {
                for (l, n) in &f.names {
                    if !x.names.iter().any(|(m, _)| m == l) {
                        x.names.push((*l, n.clone()));
                    }
                }
            }
            None => out.push(SystemFont { family: f.family.clone(), names: f.names.clone() }),
        }
    }
    out.sort_by_cached_key(|f| f.family.to_lowercase());
    out
}

pub(crate) fn scanned() -> &'static [ScannedFace] {
    static SCAN: OnceLock<Vec<ScannedFace>> = OnceLock::new();
    SCAN.get_or_init(|| {
        let mut files = Vec::new();
        for dir in font_dirs() {
            collect(&dir, 0, &mut files);
        }
        files.sort();
        files.dedup();
        // Opening a file is most of the cost (hundreds of them): a few threads open them at once.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
        let chunk = files.len().div_ceil(threads).max(1);
        let read = |c: &[PathBuf]| c.iter().flat_map(|p| faces_in(p)).collect::<Vec<_>>();
        std::thread::scope(|s| {
            let parts: Vec<_> = files
                .chunks(chunk)
                .map(|c| {
                    std::thread::Builder::new()
                        .name("novaui-font-scan".into())
                        .spawn_scoped(s, move || read(c))
                        .map_err(|_| c)
                })
                .collect();
            // A thread that did not start: its share is read here.
            parts.into_iter().flat_map(|h| h.map_or_else(read, |h| h.join().unwrap_or_default())).collect()
        })
    })
}

#[cfg(windows)]
fn font_dirs() -> Vec<PathBuf> {
    let mut d =
        vec![std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from).join("Fonts")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        d.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
    }
    d
}

#[cfg(not(windows))]
fn font_dirs() -> Vec<PathBuf> {
    let mut d = vec![PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")];
    if let Some(home) = std::env::var_os("HOME") {
        d.push(PathBuf::from(&home).join(".local/share/fonts"));
        d.push(PathBuf::from(home).join(".fonts"));
    }
    d
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 6 {
                collect(&p, depth + 1, out);
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| ["ttf", "otf", "ttc", "otc"].contains(&x.to_ascii_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_at(f: &mut File, at: u64, len: usize) -> Option<Vec<u8>> {
    let mut b = vec![0; len];
    f.seek(SeekFrom::Start(at)).ok()?;
    f.read_exact(&mut b).ok()?;
    Some(b)
}

pub(crate) fn faces_in(path: &Path) -> Vec<ScannedFace> {
    let Ok(mut f) = File::open(path) else { return Vec::new() };
    let Some(head) = read_at(&mut f, 0, 12) else { return Vec::new() };
    let offsets: Vec<u32> = if &head[..4] == b"ttcf" {
        let n = be32(&head, 8).unwrap_or(0).min(64) as usize;
        let Some(t) = read_at(&mut f, 12, 4 * n) else { return Vec::new() };
        (0..n).filter_map(|i| be32(&t, 4 * i)).collect()
    } else {
        vec![0]
    };
    offsets.iter().enumerate().filter_map(|(i, &off)| face_at(&mut f, path, i as u32, off as u64)).collect()
}

fn face_at(f: &mut File, path: &Path, index: u32, off: u64) -> Option<ScannedFace> {
    let hdr = read_at(f, off, 12)?;
    let n = be16(&hdr, 4)? as usize;
    let dir = read_at(f, off + 12, 16 * n)?;
    let table = |tag: &[u8; 4]| {
        (0..n)
            .find(|i| &dir[16 * i..16 * i + 4] == tag)
            .and_then(|i| Some((be32(&dir, 16 * i + 8)?, be32(&dir, 16 * i + 12)?)))
    };
    let (name_at, name_len) = table(b"name")?;
    let name = read_at(f, name_at as u64, (name_len as usize).min(1 << 20))?;
    let (names, sub) = family_names(&name);
    let family = names.iter().find(|(l, _)| *l == 0x0409).or(names.first())?.1.clone();
    if family.starts_with('@') {
        return None;
    }
    let (mut weight, mut italic, mut normal_width) =
        (400, sub.to_ascii_lowercase().contains("italic") || sub.to_ascii_lowercase().contains("oblique"), true);
    if let Some((at, len)) = table(b"OS/2") {
        if let Some(os2) = read_at(f, at as u64, (len as usize).min(64)) {
            weight = be16(&os2, 4).filter(|w| (1..=1000).contains(w)).unwrap_or(400);
            normal_width = be16(&os2, 6).is_none_or(|w| w == 5);
            italic |= be16(&os2, 62).is_some_and(|s| s & 1 != 0);
        }
    }
    let variable = table(b"fvar").is_some();
    Some(ScannedFace { path: path.to_path_buf(), index, family, names, weight, italic, normal_width, variable })
}

/// The family names by language (the typographic family, id 16, where the font has it, else
/// id 1), and the English subfamily (id 2: "Italic", "Bold Italic").
fn family_names(t: &[u8]) -> (Vec<(u16, String)>, String) {
    let (Some(count), Some(base)) = (be16(t, 2), be16(t, 4)) else { return (Vec::new(), String::new()) };
    let (mut typo, mut plain, mut sub) = (Vec::new(), Vec::new(), String::new());
    for i in 0..count as usize {
        let r = 6 + 12 * i;
        let (Some(platform), Some(enc), Some(lang), Some(id), Some(len), Some(at)) =
            (be16(t, r), be16(t, r + 2), be16(t, r + 4), be16(t, r + 6), be16(t, r + 8), be16(t, r + 10))
        else {
            break;
        };
        // Windows names, UTF-16BE (Unicode BMP, or full repertoire).
        if platform != 3 || !(enc == 1 || enc == 10) || !matches!(id, 1 | 2 | 16) {
            continue;
        }
        let at = base as usize + at as usize;
        let Some(raw) = t.get(at..at + len as usize) else { continue };
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&units).trim().to_string();
        if s.is_empty() {
            continue;
        }
        match id {
            16 => typo.push((lang, s)),
            1 => plain.push((lang, s)),
            _ if lang == 0x0409 => sub = s,
            _ => {}
        }
    }
    (if typo.is_empty() { plain } else { typo }, sub)
}
