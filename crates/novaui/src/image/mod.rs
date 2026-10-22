//! Images: immutable premultiplied RGBA8 pixels behind a refcounted handle; a gain-map JPEG's
//! also carry its gain map, which an HDR output shows them raised by (`gainmap`).
//!
//! Equal pixels share one handle, so they share one texture: there is nothing to alias. The tree
//! uploads a handle the first time an element draws it and frees its texture in the first frame
//! after the last clone is dropped (an element's content, the app's own cache). No app-chosen
//! ids, no free calls. Encoded bytes become a handle through [`decode`].

pub(crate) mod cache;
mod decode;
pub mod effect;
pub(crate) mod gainmap;
pub(crate) mod resample;

pub use decode::{decode, Fit};

/// `bytes` are a gain-map JPEG (Ultra HDR): an HDR display can show them brighter than SDR
/// white (`ImageContent::hdr`). A copy kept of such a picture keeps its bytes, not a re-encoding.
pub fn has_gain_map(bytes: &[u8]) -> bool {
    gainmap::find(bytes).is_some()
}

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

struct Data {
    id: u32,
    w: i32,
    h: i32,
    px: Vec<u8>,
    gain: Option<gainmap::Gain>,
}

/// A shared, immutable image. Clones are cheap; equality is identity, which for interned pixels
/// is the same as equal content.
#[derive(Clone)]
pub struct ImageHandle(Arc<Data>);

/// A handle that does not keep the pixels alive (caches, the tree's own registry).
#[derive(Clone)]
pub struct WeakImage(Weak<Data>);

/// (width, height, content hash) bucket for exact-byte interning.
type Key = (i32, i32, u64);
#[derive(Default)]
struct Pool {
    buckets: HashMap<Key, Vec<Weak<Data>>>,
    insertions: usize,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Images whose pixels went (their last handle dropped), on any thread: a tree checks its own
/// registry only when this moved, not every turn.
pub(crate) static DROP_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Drop for Data {
    fn drop(&mut self) {
        DROP_VERSION.fetch_add(1, Ordering::Release);
    }
}

impl Pool {
    fn intern(&mut self, key: Key, w: i32, h: i32, px: Vec<u8>, gain: Option<gainmap::Gain>) -> ImageHandle {
        let candidates = self.buckets.entry(key).or_default();
        candidates.retain(|weak| weak.strong_count() != 0);
        if let Some(same) = candidates.iter().filter_map(Weak::upgrade).find(|c| c.px == px && c.gain == gain) {
            return ImageHandle(same);
        }
        let data = Arc::new(Data { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), w, h, px, gain });
        candidates.push(Arc::downgrade(&data));
        self.insertions += 1;
        if self.insertions.is_multiple_of(128) {
            self.buckets.retain(|_, entries| {
                entries.retain(|weak| weak.strong_count() != 0);
                !entries.is_empty()
            });
        }
        ImageHandle(data)
    }
}

impl ImageHandle {
    /// Premultiplied RGBA8, tightly packed, row-major. Equal pixels give the same handle.
    pub fn rgba8(width: i32, height: i32, pixels: Vec<u8>) -> Self {
        Self::with_gain(width, height, pixels, None)
    }

    /// `rgba8` with a gain map fitted to the same size (`gainmap`).
    pub(crate) fn with_gain(width: i32, height: i32, pixels: Vec<u8>, gain: Option<gainmap::Gain>) -> Self {
        assert!(width > 0 && height > 0, "image dimensions must be positive");
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .expect("image dimensions overflow");
        assert_eq!(pixels.len(), expected, "image must be tightly packed RGBA8");
        let mut hash = DefaultHasher::new();
        pixels.hash(&mut hash);
        let key = (width, height, hash.finish());
        static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
        POOL.get_or_init(|| Mutex::new(Pool::default())).lock().unwrap().intern(key, width, height, pixels, gain)
    }
    pub fn width(&self) -> i32 {
        self.0.w
    }
    pub fn height(&self) -> i32 {
        self.0.h
    }
    pub fn size(&self) -> (i32, i32) {
        (self.0.w, self.0.h)
    }
    pub fn pixels(&self) -> &[u8] {
        &self.0.px
    }
    /// The image carries a gain map: an HDR output can show it brighter than SDR white.
    pub fn has_gain_map(&self) -> bool {
        self.0.gain.is_some()
    }
    /// The HDR rendition for an output with `headroom` (`gainmap::Gain::hdr`): RGBA half floats,
    /// sRGB-encoded past 1.0, premultiplied. `None` without a gain map.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn hdr_pixels(&self, headroom: f32) -> Option<Vec<u16>> {
        self.0.gain.as_ref().map(|g| g.hdr(&self.0.px, headroom))
    }
    pub fn downgrade(&self) -> WeakImage {
        WeakImage(Arc::downgrade(&self.0))
    }
    /// The backend's name for this image's texture.
    pub(crate) fn id(&self) -> u32 {
        self.0.id
    }
}

impl WeakImage {
    pub fn upgrade(&self) -> Option<ImageHandle> {
        self.0.upgrade().map(ImageHandle)
    }
    pub fn alive(&self) -> bool {
        self.0.strong_count() != 0
    }
}

impl PartialEq for ImageHandle {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}
impl Eq for ImageHandle {}
impl std::fmt::Debug for ImageHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageHandle(#{} {}x{})", self.0.id, self.0.w, self.0.h)
    }
}

impl crate::image::cache::WeakValue for ImageHandle {
    type Weak = WeakImage;
    fn downgrade(&self) -> WeakImage {
        ImageHandle::downgrade(self)
    }
    fn upgrade(weak: &WeakImage) -> Option<Self> {
        weak.upgrade()
    }
    fn alive(weak: &WeakImage) -> bool {
        weak.alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_collisions_do_not_merge_pixels_and_index_does_not_pin_data() {
        let mut pool = Pool::default();
        let a = pool.intern((1, 1, 7), 1, 1, vec![1, 2, 3, 255], None);
        let b = pool.intern((1, 1, 7), 1, 1, vec![4, 5, 6, 255], None);
        let same = pool.intern((1, 1, 7), 1, 1, vec![1, 2, 3, 255], None);
        assert!(a != b);
        assert!(a == same);
        let weak = a.downgrade();
        drop(a);
        drop(same);
        assert!(weak.upgrade().is_none());
    }
}
