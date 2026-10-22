//! The tests that read crate internals (`#[cfg(test)]` probes: backend resources, cache stats):
//! unit tests of the library. The integration tests on the public API are kept in a private
//! suite.

mod image_sharing;
mod resample_triangle;
mod text_measure;
mod weak_cache;

/// Serializes device creation: the dev machine faults when a device is created while another
/// thread creates a window.
fn device_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // Before any device or window exists in this process: the layer reads it when the loader
    // first initializes, which a window's creation can already trigger.
    std::env::set_var("DISABLE_VULKAN_OBS_CAPTURE", "1");
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
