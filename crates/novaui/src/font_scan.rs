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
    /// OS/2 ulUnicodeRange1..4; the cmap, not these broad hints, decides coverage.
    pub ranges: u128,
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
        let mut dirs = font_dirs();
        dirs.sort();
        dirs.dedup();
        for dir in dirs {
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let mut dirs = vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into(), "/run/host/fonts".into()];
    if let Some(h) = &home {
        dirs.extend([h.join(".local/share/fonts"), h.join(".fonts")]);
    }
    if let Some(x) = &xdg {
        dirs.push(x.join("fonts"));
    }
    if let Some(xs) = std::env::var_os("XDG_DATA_DIRS") {
        dirs.extend(std::env::split_paths(&xs).filter(|p| p.is_absolute()).map(|p| p.join("fonts")));
    }
    if let Ok(xml) = std::fs::read_to_string("/etc/fonts/fonts.conf") {
        dirs.extend(configured_dirs(&xml, home.as_deref(), xdg.as_deref()));
    }
    dirs
}

/// The directory entries only, not the fontconfig rules or included configuration files.
#[cfg(any(not(windows), test))]
fn configured_dirs(mut xml: &str, home: Option<&Path>, xdg: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    while let Some((_, tag)) = xml.split_once('<') {
        if let Some(comment) = tag.strip_prefix("!--") {
            xml = comment.split_once("-->").map_or("", |(_, tail)| tail);
            continue;
        }
        let Some((head, tail)) = tag.split_once('>') else { break };
        xml = tail;
        let mut attr = head.split_whitespace();
        if attr.next() != Some("dir") {
            continue;
        }
        let Some((value, tail)) = xml.split_once("</dir>") else { break };
        xml = tail;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let path = if value.starts_with('/') {
            Some(PathBuf::from(value))
        } else if let Some(path) = value.strip_prefix("~/") {
            home.map(|h| h.join(path))
        } else if attr.any(|a| a == "prefix=\"xdg\"" || a == "prefix='xdg'") {
            xdg.map(|h| h.join(value))
        } else {
            Some(Path::new("/etc/fonts").join(value))
        };
        dirs.extend(path);
    }
    dirs
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

/// OpenType OS/2 Unicode block bits. Zero means the block has no assigned bit.
pub(crate) fn range_mask(c: char) -> u128 {
    let bit = match c as u32 {
        0x0000..=0x007F => 0,
        0x0080..=0x00FF => 1,
        0x0100..=0x017F => 2,
        0x0180..=0x024F => 3,
        0x0250..=0x02AF | 0x1D00..=0x1DBF => 4,
        0x02B0..=0x02FF | 0xA700..=0xA71F => 5,
        0x0300..=0x036F | 0x1DC0..=0x1DFF => 6,
        0x0370..=0x03FF => 7,
        0x2C80..=0x2CFF => 8,
        0x0400..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => 9,
        0x0530..=0x058F => 10,
        0x0590..=0x05FF => 11,
        0xA500..=0xA63F => 12,
        0x0600..=0x06FF | 0x0750..=0x077F => 13,
        0x07C0..=0x07FF => 14,
        0x0900..=0x097F => 15,
        0x0980..=0x09FF => 16,
        0x0A00..=0x0A7F => 17,
        0x0A80..=0x0AFF => 18,
        0x0B00..=0x0B7F => 19,
        0x0B80..=0x0BFF => 20,
        0x0C00..=0x0C7F => 21,
        0x0C80..=0x0CFF => 22,
        0x0D00..=0x0D7F => 23,
        0x0E00..=0x0E7F => 24,
        0x0E80..=0x0EFF => 25,
        0x10A0..=0x10FF | 0x2D00..=0x2D2F => 26,
        0x1B00..=0x1B7F => 27,
        0x1100..=0x11FF => 28,
        0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF => 29,
        0x1F00..=0x1FFF => 30,
        0x2000..=0x206F | 0x2E00..=0x2E7F => 31,
        0x2070..=0x209F => 32,
        0x20A0..=0x20CF => 33,
        0x20D0..=0x20FF => 34,
        0x2100..=0x214F => 35,
        0x2150..=0x218F => 36,
        0x2190..=0x21FF | 0x27F0..=0x27FF | 0x2900..=0x297F | 0x2B00..=0x2BFF => 37,
        0x2200..=0x22FF | 0x2A00..=0x2AFF | 0x27C0..=0x27EF | 0x2980..=0x29FF => 38,
        0x2300..=0x23FF => 39,
        0x2400..=0x243F => 40,
        0x2440..=0x245F => 41,
        0x2460..=0x24FF => 42,
        0x2500..=0x257F => 43,
        0x2580..=0x259F => 44,
        0x25A0..=0x25FF => 45,
        0x2600..=0x26FF => 46,
        0x2700..=0x27BF => 47,
        0x3000..=0x303F => 48,
        0x3040..=0x309F => 49,
        0x30A0..=0x30FF | 0x31F0..=0x31FF => 50,
        0x3100..=0x312F | 0x31A0..=0x31BF => 51,
        0x3130..=0x318F => 52,
        0xA840..=0xA87F => 53,
        0x3200..=0x32FF => 54,
        0x3300..=0x33FF => 55,
        0xAC00..=0xD7AF => 56,
        0x10900..=0x1091F => 58,
        0x4E00..=0x9FFF | 0x2E80..=0x2FDF | 0x2FF0..=0x2FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x3190..=0x319F => {
            59
        }
        0xE000..=0xF8FF => 60,
        0x31C0..=0x31EF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F => 61,
        0xFB00..=0xFB4F => 62,
        0xFB50..=0xFDFF => 63,
        0xFE20..=0xFE2F => 64,
        0xFE10..=0xFE1F | 0xFE30..=0xFE4F => 65,
        0xFE50..=0xFE6F => 66,
        0xFE70..=0xFEFF => 67,
        0xFF00..=0xFFEF => 68,
        0xFFF0..=0xFFFF => 69,
        0x0F00..=0x0FFF => 70,
        0x0700..=0x074F => 71,
        0x0780..=0x07BF => 72,
        0x0D80..=0x0DFF => 73,
        0x1000..=0x109F => 74,
        0x1200..=0x139F | 0x2D80..=0x2DDF => 75,
        0x13A0..=0x13FF => 76,
        0x1400..=0x167F => 77,
        0x1680..=0x169F => 78,
        0x16A0..=0x16FF => 79,
        0x1780..=0x17FF | 0x19E0..=0x19FF => 80,
        0x1800..=0x18AF => 81,
        0x2800..=0x28FF => 82,
        0xA000..=0xA4CF => 83,
        0x1700..=0x177F => 84,
        0x10300..=0x1032F => 85,
        0x10330..=0x1034F => 86,
        0x10400..=0x1044F => 87,
        0x1D000..=0x1D24F => 88,
        0x1D400..=0x1D7FF => 89,
        0xF0000..=0xFFFFD | 0x100000..=0x10FFFD => 90,
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => 91,
        0xE0000..=0xE007F => 92,
        0x1900..=0x194F => 93,
        0x1950..=0x197F => 94,
        0x1980..=0x19DF => 95,
        0x1A00..=0x1A1F => 96,
        0x2C00..=0x2C5F => 97,
        0x2D30..=0x2D7F => 98,
        0x4DC0..=0x4DFF => 99,
        0xA800..=0xA82F => 100,
        0x10000..=0x1013F => 101,
        0x10140..=0x1018F => 102,
        0x10380..=0x1039F => 103,
        0x103A0..=0x103DF => 104,
        0x10450..=0x1047F => 105,
        0x10480..=0x104AF => 106,
        0x10800..=0x1083F => 107,
        0x10A00..=0x10A5F => 108,
        0x1D300..=0x1D35F => 109,
        0x12000..=0x1247F => 110,
        0x1D360..=0x1D37F => 111,
        0x1B80..=0x1BBF => 112,
        0x1C00..=0x1C4F => 113,
        0x1C50..=0x1C7F => 114,
        0xA880..=0xA8DF => 115,
        0xA900..=0xA92F => 116,
        0xA930..=0xA95F => 117,
        0xAA00..=0xAA5F => 118,
        0x10190..=0x101CF => 119,
        0x101D0..=0x101FF => 120,
        0x102A0..=0x102DF | 0x10280..=0x1029F | 0x10920..=0x1093F => 121,
        0x1F000..=0x1F09F => 122,
        _ if c as u32 > 0xffff => 57,
        _ => return 0,
    };
    1 << bit
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
    let mut ranges = 0;
    if let Some((at, len)) = table(b"OS/2") {
        if let Some(os2) = read_at(f, at as u64, (len as usize).min(64)) {
            weight = be16(&os2, 4).filter(|w| (1..=1000).contains(w)).unwrap_or(400);
            normal_width = be16(&os2, 6).is_none_or(|w| w == 5);
            for i in 0..4 {
                ranges |= u128::from(be32(&os2, 42 + 4 * i).unwrap_or(0)) << (32 * i);
            }
            italic |= be16(&os2, 62).is_some_and(|s| s & 1 != 0);
        }
    }
    let variable = table(b"fvar").is_some();
    Some(ScannedFace { path: path.to_path_buf(), index, family, names, weight, italic, normal_width, variable, ranges })
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fontconfig_directories_expand_their_prefixes_and_ignore_comments() {
        let xml = r#"<!-- <dir>/skip</dir> --><fontconfig><dir> /opt/type </dir><dir prefix="xdg">fonts</dir><dir>~/.fonts</dir></fontconfig>"#;
        assert_eq!(
            configured_dirs(xml, Some(Path::new("/home/test")), Some(Path::new("/data"))),
            [PathBuf::from("/opt/type"), PathBuf::from("/data/fonts"), PathBuf::from("/home/test/.fonts")]
        );
    }
}
