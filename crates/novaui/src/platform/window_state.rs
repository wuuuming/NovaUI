//! Window-manager state changes are not continuous border resizing.
use windows::Win32::UI::WindowsAndMessaging::{SIZE_MAXIMIZED, SIZE_MINIMIZED, SIZE_RESTORED};

#[derive(Default)]
pub(super) struct WindowState {
    pub minimized: bool,
    maximized: bool,
}
pub(super) enum SizeUpdate {
    Ignore,
    Minimized,
    Apply { width: i32, height: i32, maximized: bool, instant: bool },
}
impl WindowState {
    pub fn resize(&mut self, kind: u32, width: i32, height: i32) -> SizeUpdate {
        if kind == SIZE_MINIMIZED {
            self.minimized = true;
            // The LPARAM size is not a usable content viewport, even if it is nonzero.
            return SizeUpdate::Minimized;
        }
        if !matches!(kind, SIZE_RESTORED | SIZE_MAXIMIZED) || width <= 0 || height <= 0 {
            return SizeUpdate::Ignore;
        }
        let maximized = kind == SIZE_MAXIMIZED;
        let instant = self.minimized || maximized != self.maximized;
        self.minimized = false;
        self.maximized = maximized;
        SizeUpdate::Apply { width, height, maximized, instant }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn minimized_dimensions_never_become_layout_sizes() {
        let mut state = WindowState::default();
        assert!(matches!(state.resize(SIZE_RESTORED, 1280, 720), SizeUpdate::Apply { instant: false, .. }));
        for (w, h) in [(0, 0), (160, 28), (1, 1)] {
            assert!(matches!(state.resize(SIZE_MINIMIZED, w, h), SizeUpdate::Minimized));
            assert!(state.minimized);
        }
        assert!(matches!(state.resize(SIZE_RESTORED, 1280, 720), SizeUpdate::Apply { instant: true, .. }));
        assert!(!state.minimized);
        assert!(matches!(state.resize(SIZE_RESTORED, 1200, 700), SizeUpdate::Apply { instant: false, .. }));
        assert!(matches!(
            state.resize(SIZE_MAXIMIZED, 2560, 1440),
            SizeUpdate::Apply { instant: true, maximized: true, .. }
        ));
        state.resize(SIZE_MINIMIZED, 160, 28);
        assert!(matches!(state.resize(SIZE_MAXIMIZED, 2560, 1440), SizeUpdate::Apply { instant: true, .. }));
        assert!(matches!(
            state.resize(SIZE_RESTORED, 1280, 720),
            SizeUpdate::Apply { instant: true, maximized: false, .. }
        ));
    }
}
