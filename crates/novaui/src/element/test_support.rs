//! Structural assertions for the unit-test harness, never compiled into the product library.
use super::Tree;

impl Tree {
    /// Bytes of image pixels the tree has on the backend (live handles only).
    pub(crate) fn image_pixel_capacity(&self) -> usize {
        self.images.values().filter_map(|u| u.weak.upgrade()).map(|h| h.pixels().len()).sum()
    }
}
