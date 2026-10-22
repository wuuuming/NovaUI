//! Which output the window is on, HDR or SDR (`Backend::set_output`). Win32 has no event for a
//! change of it: the host asks again when the window may have moved to another monitor, when the
//! display configuration changed, when the window is activated (the SDR white slider in Settings
//! sends nothing), and whenever DXGI says its view of the outputs is stale (`IsCurrent`).
//!
//! The output is the DXGI one of the monitor the window is mostly on; it is HDR when its colour
//! space is BT.2100 PQ (`IDXGIOutput6::GetDesc1`). The SDR white is the display configuration's
//! (`DISPLAYCONFIG_SDR_WHITE_LEVEL`), the peak the display's own report (`MaxLuminance`).
//! `NOVAUI_HDR=0` keeps every output SDR.

use crate::render::primitive::HdrOutput;
use windows::core::Interface;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::*;

/// The host's view of the outputs: the factory whose staleness says they changed.
#[derive(Default)]
pub(super) struct Outputs {
    factory: Option<IDXGIFactory1>,
}

impl Outputs {
    /// DXGI's outputs changed since the last `of` (a display turned HDR on or off).
    pub(super) fn stale(&self) -> bool {
        // SAFETY: a plain query on our factory.
        self.factory.as_ref().is_some_and(|f| !unsafe { f.IsCurrent() }.as_bool())
    }

    /// The HDR output `hwnd` is mostly on; `None` for SDR (or when it cannot be told).
    pub(super) fn of(&mut self, hwnd: HWND) -> Option<HdrOutput> {
        if std::env::var_os("NOVAUI_HDR").is_some_and(|v| v == "0") {
            return None;
        }
        // SAFETY: DXGI and display-configuration queries on this thread; every structure passed
        // is a live local with its size set where the call reads one.
        unsafe {
            // A new factory each time: an old one keeps describing the outputs as they were.
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            self.factory = Some(factory.clone());
            let monitor = super::win32::monitor_of(hwnd);
            let output = super::win32::vblank::output_in(&factory, monitor)?;
            let d = output.cast::<IDXGIOutput6>().and_then(|o6| o6.GetDesc1()).ok()?;
            if d.ColorSpace != DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
                return None;
            }
            // 80 nits is Windows' nominal SDR white; 200 is its usual setting.
            let white = sdr_white(&d.DeviceName).unwrap_or(200.0);
            // A display that reports no peak (or one under its white) has no headroom.
            let peak = if d.MaxLuminance.is_finite() { d.MaxLuminance.max(white) } else { white };
            Some(HdrOutput { sdr_white: white, peak })
        }
    }
}

/// The SDR white level, in nits, of the display GDI names `device` (`\\.\DISPLAY1`).
fn sdr_white(device: &[u16; 32]) -> Option<f32> {
    // SAFETY: the buffers are sized by GetDisplayConfigBufferSizes and passed with their lengths;
    // each request packet is a live local whose header names its own type and size.
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).ok().ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            .ok()
            .ok()?;
        for p in &paths[..np as usize] {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0 || name.viewGdiDeviceName != *device {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 {
                return None;
            }
            // Thousandths of 80 nits.
            return Some(white.SDRWhiteLevel as f32 / 1000.0 * 80.0).filter(|w| *w >= 80.0);
        }
        None
    }
}
