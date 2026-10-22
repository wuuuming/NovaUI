//! Debug contracts (`runtime.md`, "How this is enforced"): the phase rules ("What each phase may
//! do") and the damage audit (`Tree::frame`). A broken one panics.
//!
//! On in debug builds. `NOVAUI_AUDIT=1` turns them on in any build (CI runs its release tests
//! so), `NOVAUI_AUDIT=0` off.

pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| match std::env::var("NOVAUI_AUDIT").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(debug_assertions),
    })
}

/// `assert!` while the contracts are on (`on`).
macro_rules! contract {
    ($cond:expr, $($arg:tt)+) => {
        if $crate::audit::on() {
            assert!($cond, $($arg)+);
        }
    };
}
pub(crate) use contract;
