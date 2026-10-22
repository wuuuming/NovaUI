//! The display's refresh inside Win32's modal size/move loop.
//!
//! While the user drags the border, Win32 runs its own message loop and ours does not run. A
//! window timer cannot stand in for it: timers tick on the system clock (~15.6 ms), so an
//! animation between two border steps (the layout tween after a column switch) ran at ~64 Hz
//! whatever the display. This thread waits for each vertical blank of the window's monitor
//! (`IDXGIOutput::WaitForVBlank`; a sleep of one refresh when the monitor has no DXGI output)
//! and posts the window one [`WM_APP_VBLANK`], which the modal loop dispatches: one turn per
//! refresh. It runs only while the loop does, and never has more than one post outstanding.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted once per refresh while the size/move loop runs.
pub(super) const WM_APP_VBLANK: u32 = WM_APP + 2;

/// The running thread's flags.
pub(super) struct Vblank {
    stop: Arc<AtomicBool>,
    /// A post is in the window's queue, not yet handled.
    posted: Arc<AtomicBool>,
}

impl Vblank {
    /// Starts posting to `hwnd` at each refresh of `monitor` (`period` seconds when it has no
    /// DXGI output).
    pub(super) fn start(hwnd: HWND, monitor: HMONITOR, period: f64) -> Vblank {
        let stop = Arc::new(AtomicBool::new(false));
        let posted = Arc::new(AtomicBool::new(false));
        let (h, m) = (hwnd.0 as isize, monitor.0 as isize);
        let (s, p) = (stop.clone(), posted.clone());
        let period = Duration::from_secs_f64(period.clamp(1.0 / 1000.0, 1.0 / 30.0));
        let _ = std::thread::Builder::new().name("novaui-vblank".into()).spawn(move || {
            let output = output_of(HMONITOR(m as _));
            while !s.load(Ordering::Acquire) {
                // SAFETY: an output this thread created and alone uses.
                let waited = output.as_ref().is_some_and(|o| unsafe { o.WaitForVBlank() }.is_ok());
                if !waited {
                    std::thread::sleep(period);
                }
                if !s.load(Ordering::Acquire) && !p.swap(true, Ordering::AcqRel) {
                    // SAFETY: a plain post; a window already destroyed makes it fail, harmlessly.
                    let _ = unsafe { PostMessageW(HWND(h as _), WM_APP_VBLANK, WPARAM(0), LPARAM(0)) };
                }
            }
        });
        Vblank { stop, posted }
    }

    /// The window took the post: the next refresh may post again.
    pub(super) fn handled(&self) {
        self.posted.store(false, Ordering::Release);
    }
}

impl Drop for Vblank {
    fn drop(&mut self) {
        // The thread ends after its current wait, at most one refresh.
        self.stop.store(true, Ordering::Release);
    }
}

/// The DXGI output showing `monitor`, if any.
fn output_of(monitor: HMONITOR) -> Option<IDXGIOutput> {
    // SAFETY: a factory made and used on this thread.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    output_in(&factory, monitor)
}

/// The DXGI output of `factory`'s adapters that shows `monitor`.
pub(in crate::platform) fn output_in(factory: &IDXGIFactory1, monitor: HMONITOR) -> Option<IDXGIOutput> {
    // SAFETY: the adapters and outputs `factory` enumerates, used on this thread; the
    // description is a plain out-structure.
    unsafe {
        for a in 0.. {
            let adapter = factory.EnumAdapters1(a).ok()?;
            for o in 0.. {
                let Ok(output) = adapter.EnumOutputs(o) else { break };
                if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                    return Some(output);
                }
            }
        }
        None
    }
}
