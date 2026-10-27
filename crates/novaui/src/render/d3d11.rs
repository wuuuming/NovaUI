//! D3D11 backend. Canvas and layers are RGBA8 premultiplied textures; every draw scissors to the
//! damage rectangle and issues one instanced draw per run of equal primitives. Presentation copies
//! only damage into a flip-model swap chain (plus the previous frame's damage, for the other
//! buffer) and calls `Present1` with dirty rectangles. The host waits on the frame-latency object.
//! The swap chain is shown through DirectComposition (`compose`) or is the window's (`attach`).
//!
//! On an HDR output (`set_output`) the canvas, layers and blur chain are half floats holding the
//! same sRGB-encoded colour, an HDR image's above 1.0; the swap chain is half floats in scRGB
//! (linear, 1.0 = 80 nits), and the present decodes each pixel and scales it to the output's SDR
//! white (`fs_present`) instead of copying it.

use crate::geom::*;
use crate::render::batch::{self as gpu, Batch, Inst, Tex as TexRef, INST_SIZE};
use crate::render::primitive::*;
use std::collections::HashMap;
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::{E_FAIL, E_POINTER, HANDLE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::{IDCompositionDevice, IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub const ATLAS: i32 = crate::text::ATLAS;

struct Tex {
    tex: ID3D11Texture2D,
    rtv: Option<ID3D11RenderTargetView>,
    srv: ID3D11ShaderResourceView,
    w: i32,
    h: i32,
}

/// Textures of large images, made on a thread of their own: creating a 480 x 480 cover's texture
/// took 0.8-1.2 ms of the UI thread. The device is free-threaded (not created
/// `SINGLETHREADED`), so the thread makes them with it directly; the UI thread takes them in
/// at the start of a turn (`images_made`).
struct Maker {
    /// The image, its wake, and the headroom its HDR rendition is made for (an HDR output's).
    jobs: std::sync::mpsc::Sender<(crate::image::ImageHandle, Wake, Option<f32>)>,
    done: std::sync::mpsc::Receiver<(u32, Option<Made>)>,
    /// Asked for and not taken in yet. An image freed meanwhile leaves the set, and its texture
    /// is dropped when it comes.
    making: std::collections::HashSet<u32>,
}

/// A texture the maker's thread made, and its HDR rendition's.
struct Made(Tex, Option<Tex>);
/// The device, for the maker's thread.
struct FreeDevice(ID3D11Device);
// SAFETY: the device is free-threaded (created without `D3D11_CREATE_DEVICE_SINGLETHREADED`):
// its methods, and the resources and views it makes, may be used and released on any thread.
unsafe impl Send for Made {}
// SAFETY: as for `Made`.
unsafe impl Send for FreeDevice {}

impl Maker {
    fn start(dev: &ID3D11Device) -> Option<Maker> {
        let (jobs, rx) = std::sync::mpsc::channel::<(crate::image::ImageHandle, Wake, Option<f32>)>();
        let (tx, done) = std::sync::mpsc::channel();
        let dev = FreeDevice(dev.clone());
        std::thread::Builder::new()
            .name("novaui-images".into())
            .spawn(move || {
                let dev = dev;
                // Ends when the backend (the sender) goes.
                for (img, wake, headroom) in rx {
                    let (w, h) = (img.width(), img.height());
                    let t = D3d11::image_tex(&dev.0, w, h, img.pixels())
                        .ok()
                        .map(|t| Made(t, headroom.and_then(|hr| hdr_tex(&dev.0, &img, hr))));
                    if tx.send((img.id(), t)).is_err() {
                        return;
                    }
                    wake();
                }
            })
            .ok()?;
        Some(Maker { jobs, done, making: Default::default() })
    }
}

pub struct D3d11 {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    /// One per program; `None` for an image effect that did not compile (its draws are skipped).
    ps: Vec<Option<ID3D11PixelShader>>,
    /// Image effects `ps` covers (`image::effect`); more registered later are added at the next
    /// draw.
    effects: usize,
    layout: ID3D11InputLayout,
    blend_over: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    cb: ID3D11Buffer,
    vb: ID3D11Buffer,
    vb_cap: usize,
    atlas: Tex,
    canvas: Tex,
    layers: HashMap<u32, Tex>,
    images: HashMap<u32, Tex>,
    /// Images' HDR renditions (half floats), made on an HDR output for images with a gain map.
    hdr_images: HashMap<u32, Tex>,
    /// The output is HDR: targets are `fmt` (half floats), the swap chain scRGB.
    hdr: Option<HdrOutput>,
    /// The canvas's, layers' and blur chain's format.
    fmt: DXGI_FORMAT,
    /// The HDR present's program (`fs_present`).
    present_ps: ID3D11PixelShader,
    /// Makes large images' textures off the UI thread; started by the first.
    maker: Option<Maker>,
    /// The material's blur chain (level 0 = the backdrop region), grown on demand in buckets.
    blur: Vec<Tex>,
    insts: Vec<Inst>,
    batches: Vec<Batch>,
    swap: Option<IDXGISwapChain1>,
    /// The swap chain's buffer size: the window's client size for a window swap chain; at least
    /// that for a composition one (`composition_size`).
    swap_wh: (i32, i32),
    /// Frames reach the screen through DirectComposition (`compose`), else through a swap chain
    /// on the window (`attach`).
    comp: Option<Composition>,
    /// The swap chain's buffers were made again and nothing has been presented into them yet:
    /// `end_frame` presents the canvas.
    unshown: bool,
    /// A texture was released this frame: flush so the driver frees it now (D3D11 destroys
    /// lazily; without a flush a drag through many sizes kept every freed buffer alive).
    released: bool,
    latency: Option<HANDLE>,
    /// Signals of the frame-latency object not yet consumed: one per present (and one for the
    /// swap chain's start). The object is a semaphore released as each present retires; a
    /// present made without a wait (a live resize frame) still releases it. Consuming only one
    /// signal per wait then leaves the surplus counted up: after a resize storm, every wait
    /// returned at once and dozens of frames went into the present queue in a few ms, each
    /// shown a refresh later, so the next animation played in slow motion.
    owed: u32,
    /// When the newest present was made: a signal not there 34 ms after it will not come.
    presented_at: std::time::Instant,
    first_present: bool,
    prev_damage: Vec<Rect>,
    adapter: String,
    lost: bool,
    immediate: bool,
    /// The canvas's used size (its texture is allocated in 256-px buckets).
    cw: i32,
    ch: i32,
    /// `finish`'s wait (made on first use).
    completion: Option<Completion>,
}

/// How `finish` waits for the GPU: a fence whose completion sets an event the thread sleeps on
/// (D3D11.4); else an event query polled with short sleeps.
enum Completion {
    Fence { fence: ID3D11Fence, ctx: ID3D11DeviceContext4, event: Event, value: u64 },
    Query(ID3D11Query),
}

/// An owned Win32 event handle.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own, from `CreateEventW`, closed once here.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// The DirectComposition objects that show a composition swap chain in a window. The target binds
/// the visual to the window until it is dropped; a window takes one target.
struct Composition {
    device: IDCompositionDevice,
    visual: IDCompositionVisual,
    /// Set by `compose_into`.
    target: Option<(IDCompositionTarget, HWND)>,
}

/// `DCompositionCreateDevice` (Windows 8+), looked up at run time: the binary starts where
/// dcomp.dll is missing, and the caller presents through the window instead.
fn dcomp_device(dxgi: &IDXGIDevice) -> windows::core::Result<IDCompositionDevice> {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    type Create = unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const windows::core::GUID,
        *mut *mut std::ffi::c_void,
    ) -> windows::core::HRESULT;
    // SAFETY: the module stays loaded for the process (never freed); the symbol is the documented
    // export with the signature above; `out` receives an owned reference to the asked interface,
    // which `from_raw` takes over.
    unsafe {
        let lib = LoadLibraryW(windows::core::w!("dcomp.dll"))?;
        let f = GetProcAddress(lib, windows::core::s!("DCompositionCreateDevice"))
            .ok_or_else(|| failed("no DCompositionCreateDevice".into()))?;
        let create: Create = std::mem::transmute(f);
        let mut out = std::ptr::null_mut();
        create(dxgi.as_raw(), &IDCompositionDevice::IID, &mut out).ok()?;
        if out.is_null() {
            return Err(E_POINTER.into());
        }
        Ok(IDCompositionDevice::from_raw(out))
    }
}

/// Shows the top-left `w`×`h` of `bw`×`bh` buffers at 1:1. A composition swap chain stretches
/// its source region over the buffer size (as a window's does under any scaling mode, which is
/// why only composition swap chains are oversized); the matrix scales it back. Both apply at the
/// next present, with its content.
fn crop(sc: &IDXGISwapChain1, w: i32, h: i32, bw: i32, bh: i32) -> windows::core::Result<()> {
    let (w, h) = (w.max(1), h.max(1));
    let sc2: IDXGISwapChain2 = sc.cast()?;
    let m = DXGI_MATRIX_3X2_F { _11: w as f32 / bw as f32, _22: h as f32 / bh as f32, ..Default::default() };
    // SAFETY: our swap chain; the region lies inside its buffers.
    unsafe {
        sc2.SetSourceSize(w as u32, h as u32)?;
        sc2.SetMatrixTransform(&m)
    }
}

/// The size of the monitor `hwnd` is mostly on.
fn monitor_size(hwnd: HWND) -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::*;
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a live local structure with its size set.
    if unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi) }.as_bool() {
        (mi.rcMonitor.right - mi.rcMonitor.left, mi.rcMonitor.bottom - mi.rcMonitor.top)
    } else {
        (0, 0)
    }
}

/// `img`'s HDR rendition for `headroom` as a half-float texture; `None` without a gain map.
fn hdr_tex(dev: &ID3D11Device, img: &crate::image::ImageHandle, headroom: f32) -> Option<Tex> {
    let px = img.hdr_pixels(headroom)?;
    let bytes: Vec<u8> = px.iter().flat_map(|v| v.to_le_bytes()).collect();
    let w = img.width();
    D3d11::tex(dev, w, img.height(), DXGI_FORMAT_R16G16B16A16_FLOAT, false, Some((&bytes, (w * 8) as u32))).ok()
}

/// Whether `t` is reused for a `w`×`h` target (`batch::fits`).
fn fits(t: &Tex, w: i32, h: i32) -> bool {
    gpu::fits((t.w, t.h), w, h)
}

/// What a D3D11 create call wrote to its out-parameter; a call that succeeded without writing
/// one is a driver fault, reported as one.
fn made<T>(out: Option<T>) -> windows::core::Result<T> {
    out.ok_or_else(|| windows::core::Error::from(E_POINTER))
}

fn failed(message: String) -> windows::core::Error {
    windows::core::Error::new(E_FAIL, message)
}

/// `r` as a copy box.
fn box_of(r: Rect) -> D3D11_BOX {
    D3D11_BOX { left: r.x as u32, top: r.y as u32, front: 0, right: r.r() as u32, bottom: r.b() as u32, back: 1 }
}

/// `r` as a scissor or dirty rectangle.
fn rect_of(r: Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.r(), bottom: r.b() }
}

/// A viewport covering a `w`×`h` target.
fn viewport(w: i32, h: i32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }
}

fn compile(src: &str, entry: &str, target: &str) -> windows::core::Result<Vec<u8>> {
    let entry_c = format!("{entry}\0");
    let target_c = format!("{target}\0");
    let mut code = None;
    let mut err = None;
    // SAFETY: `src` is read for `src.len()` bytes; `entry_c` / `target_c` are NUL-terminated and
    // outlive the call; the blobs are returned owned.
    let r = unsafe {
        D3DCompile(
            src.as_ptr() as _,
            src.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr()),
            PCSTR(target_c.as_ptr()),
            0,
            0,
            &mut code,
            Some(&mut err),
        )
    };
    if r.is_err() {
        // SAFETY: the blob's pointer and size describe its own buffer, alive while `b` is.
        let e = err.map(|b: ID3DBlob| unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()))
                .into_owned()
        });
        return Err(failed(format!("D3DCompile {entry}: {r:?} {}", e.unwrap_or_default())));
    }
    let b = made(code)?;
    // SAFETY: the blob's pointer and size describe its own buffer, alive while `b` is.
    Ok(unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() })
}

impl D3d11 {
    pub fn new(w: i32, h: i32) -> windows::core::Result<Self> {
        // SAFETY: device and resource creation on this thread; every description passed is a
        // live local (the semantic name a static C string), every out-parameter a local `Option`
        // read through `made`.
        unsafe {
            let mut dev = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                // The driver submits on this thread rather than its own worker: a UI's few draws
                // a frame cost more in the hand-off (a wake of the driver's thread per present)
                // than they save (measured: −22 % CPU at 240 fps, a quarter fewer context switches).
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )?;
            let dev: ID3D11Device = made(dev)?;
            let adapter = dev
                .cast::<IDXGIDevice>()
                .and_then(|d| d.GetAdapter())
                .and_then(|a| a.GetDesc())
                .map(|d| {
                    String::from_utf16_lossy(&d.Description[..d.Description.iter().position(|&c| c == 0).unwrap_or(0)])
                })
                .unwrap_or_default();
            let ctx: ID3D11DeviceContext = made(ctx)?;
            // The built-in programs (HLSL made when novaui was built); image effects come with the
            // first draw, one by one, so one that does not compile here costs only its own draws
            // (`draw`).
            let src = gpu::HLSL;
            let vsb = compile(src, "vs", "vs_5_0")?;
            let mut vs = None;
            dev.CreateVertexShader(&vsb, None, Some(&mut vs))?;
            let mut ps = Vec::new();
            for e in gpu::FS {
                let b = compile(src, e, "ps_5_0")?;
                let mut p = None;
                dev.CreatePixelShader(&b, None, Some(&mut p))?;
                ps.push(Some(made(p)?));
            }
            let b = compile(src, "fs_present", "ps_5_0")?;
            let mut present_ps = None;
            dev.CreatePixelShader(&b, None, Some(&mut present_ps))?;
            let sem = PCSTR(c"LOC".as_ptr().cast());
            let elems: Vec<D3D11_INPUT_ELEMENT_DESC> = (0..gpu::INST_ATTRS)
                .map(|i| D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: sem,
                    SemanticIndex: i,
                    Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: i * 16,
                    InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                    InstanceDataStepRate: 1,
                })
                .collect();
            let mut layout = None;
            dev.CreateInputLayout(&elems, &vsb, Some(&mut layout))?;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend_over = None;
            dev.CreateBlendState(&bd, Some(&mut blend_over))?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                ScissorEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            dev.CreateRasterizerState(&rd, Some(&mut raster))?;
            let cbd = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            dev.CreateBuffer(&cbd, None, Some(&mut cb))?;
            let atlas = Self::tex(&dev, ATLAS, ATLAS, DXGI_FORMAT_R8_UNORM, false, None)?;
            let canvas = Self::tex(&dev, gpu::bucket(w), gpu::bucket(h), DXGI_FORMAT_R8G8B8A8_UNORM, true, None)?;
            let vb_cap = 4096;
            let vb = Self::vbuf(&dev, vb_cap)?;
            Ok(D3d11 {
                dev,
                ctx,
                vs: made(vs)?,
                ps,
                effects: 0,
                layout: made(layout)?,
                blend_over: made(blend_over)?,
                raster: made(raster)?,
                cb: made(cb)?,
                vb,
                vb_cap,
                atlas,
                canvas,
                layers: HashMap::new(),
                images: HashMap::new(),
                hdr_images: HashMap::new(),
                hdr: None,
                fmt: DXGI_FORMAT_R8G8B8A8_UNORM,
                present_ps: made(present_ps)?,
                maker: None,
                blur: Vec::new(),
                insts: Vec::new(),
                batches: Vec::new(),
                swap: None,
                comp: None,
                unshown: false,
                latency: None,
                swap_wh: (0, 0),
                released: false,
                owed: 0,
                presented_at: std::time::Instant::now(),
                first_present: true,
                prev_damage: Vec::new(),
                adapter,
                lost: false,
                immediate: false,
                cw: w,
                ch: h,

                completion: None,
            })
        }
    }

    fn vbuf(dev: &ID3D11Device, n: usize) -> windows::core::Result<ID3D11Buffer> {
        let d = D3D11_BUFFER_DESC {
            ByteWidth: (n * INST_SIZE) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let mut b = None;
        // SAFETY: `d` and `b` are live locals; no initial data.
        unsafe { dev.CreateBuffer(&d, None, Some(&mut b))? };
        made(b)
    }

    fn tex(
        dev: &ID3D11Device,
        w: i32,
        h: i32,
        fmt: DXGI_FORMAT,
        rt: bool,
        init: Option<(&[u8], u32)>,
    ) -> windows::core::Result<Tex> {
        let d = D3D11_TEXTURE2D_DESC {
            Width: w.max(1) as u32,
            Height: h.max(1) as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | if rt { D3D11_BIND_RENDER_TARGET.0 } else { 0 }) as u32,
            ..Default::default()
        };
        // The initial data is read for `Height` rows of `pitch` bytes: it must hold them.
        if let Some((p, pitch)) = init {
            if (p.len() as u64) < pitch as u64 * d.Height as u64 {
                return Err(failed(format!("texture data: {} bytes for {w}x{h} at pitch {pitch}", p.len())));
            }
        }
        // SAFETY: `d` and the out-parameters are live locals; the initial data, when given, holds
        // `Height` rows of `pitch` bytes (checked above) and outlives the call, which copies it.
        unsafe {
            let mut t = None;
            let sd = init.map(|(p, pitch)| D3D11_SUBRESOURCE_DATA {
                pSysMem: p.as_ptr() as _,
                SysMemPitch: pitch,
                SysMemSlicePitch: 0,
            });
            dev.CreateTexture2D(&d, sd.as_ref().map(|s| s as *const _), Some(&mut t))?;
            let t: ID3D11Texture2D = made(t)?;
            let mut srv = None;
            dev.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            let mut rtv = None;
            if rt {
                dev.CreateRenderTargetView(&t, None, Some(&mut rtv))?;
            }
            Ok(Tex { tex: t, rtv, srv: made(srv)?, w, h })
        }
    }

    /// An image's texture: RGBA8 premultiplied, `w * 4` bytes a row, not a render target.
    fn image_tex(dev: &ID3D11Device, w: i32, h: i32, rgba: &[u8]) -> windows::core::Result<Tex> {
        Self::tex(dev, w, h, DXGI_FORMAT_R8G8B8A8_UNORM, false, Some((rgba, (w * 4) as u32)))
    }

    fn acquire_target(&mut self, w: i32, h: i32) -> windows::core::Result<Tex> {
        Self::tex(&self.dev, gpu::bucket(w), gpu::bucket(h), self.fmt, true, None)
    }

    /// The swap chain's buffers in the format and colour space the output takes: half floats in
    /// scRGB on an HDR output, else RGBA8 sRGB. `false` when the swap chain cannot show that
    /// colour space.
    fn swap_format(&mut self, hdr: bool) -> bool {
        // Headless (no swap chain): only the targets change.
        let Some(sc) = self.swap.clone() else { return true };
        let (fmt, space) = if hdr {
            (DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)
        } else {
            (DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)
        };
        // SAFETY: the immediate context through `&mut self`; unbinding first leaves no reference
        // to the buffers ResizeBuffers releases (`present` holds none past its call).
        unsafe { self.ctx.OMSetRenderTargets(None, None) };
        let (bw, bh) = self.swap_wh;
        // SAFETY: our swap chain, no buffer of it bound or held.
        let r = unsafe {
            sc.ResizeBuffers(0, bw as u32, bh as u32, fmt, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)
        };
        if r.is_err() {
            self.check(r);
            return false;
        }
        self.buffers_remade();
        self.show_whole();
        let Ok(sc3) = sc.cast::<IDXGISwapChain3>() else { return !hdr };
        // SAFETY: our swap chain's colour space, asked about and set on this thread.
        let ok = unsafe {
            let flags = sc3.CheckColorSpaceSupport(space).unwrap_or(0);
            flags & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32 != 0 && sc3.SetColorSpace1(space).is_ok()
        };
        if self.comp.is_some() {
            let r = crop(&sc, self.cw, self.ch, bw, bh);
            self.check(r);
        }
        ok
    }

    /// Takes the latency object's signals now: presents the buffers' remaking discarded never
    /// release theirs (`size_swap`).
    fn drain_latency(&mut self) {
        if let Some(h) = self.latency {
            // SAFETY: the swap chain's own latency handle, alive with `self.swap`.
            while unsafe { windows::Win32::System::Threading::WaitForSingleObjectEx(h, 0, false) }
                == windows::Win32::Foundation::WAIT_OBJECT_0
            {}
        }
        self.owed = 0;
    }

    /// The HDR present: each rectangle drawn from the canvas into the back buffer by
    /// `fs_present` (decoded, scaled by `white`, SDR white over 80 nits) instead of copied.
    fn present_hdr(&mut self, bb: &ID3D11Texture2D, rects: &[Rect], white: f32) {
        let insts: Vec<Inst> = rects
            .iter()
            .map(|r| Inst {
                dst: [r.x as f32, r.y as f32, r.w as f32, r.h as f32],
                c0: [white, 0.0, 0.0, 0.0],
                ..Default::default()
            })
            .collect();
        if insts.is_empty() || !self.upload(&insts) {
            return;
        }
        let (bw, bh) = self.swap_wh;
        // SAFETY: the immediate context through `&mut self` on this thread; the view is made of
        // the swap chain's current back buffer and released before the present; `g` is the
        // constant buffer's 16 bytes; the instances were just uploaded.
        unsafe {
            let mut rtv = None;
            if let Err(e) = self.dev.CreateRenderTargetView(bb, None, Some(&mut rtv)) {
                self.check(Err(e));
                return;
            }
            let ctx = self.ctx.clone();
            let g = [bw as f32, bh as f32, 0.0, 0.0];
            ctx.UpdateSubresource(&self.cb, 0, None, g.as_ptr() as _, 0, 0);
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.OMSetRenderTargets(Some(&[rtv]), None);
            ctx.RSSetViewports(Some(&[viewport(bw, bh)]));
            ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: bw, bottom: bh }]));
            self.bind_quads(&ctx);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.PSSetShader(&self.present_ps, None);
            ctx.PSSetShaderResources(1, Some(&[Some(self.atlas.srv.clone()), Some(self.canvas.srv.clone()), None]));
            ctx.DrawInstanced(4, insts.len() as u32, 0, 0);
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }
    /// Binds the instanced-quad pipeline every draw uses: rasterizer, topology, input layout,
    /// the instance buffer, the vertex shader and the constants.
    ///
    /// # Safety
    /// `ctx` is the immediate context, used only through `&mut self` on this thread.
    unsafe fn bind_quads(&self, ctx: &ID3D11DeviceContext) {
        // SAFETY: the caller's; stride and offset are live locals.
        unsafe {
            ctx.RSSetState(&self.raster);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.layout);
            let stride = INST_SIZE as u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(self.vb.clone())), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
        }
    }

    fn retire_target(&mut self, _target: Tex) {
        self.released = true;
    }

    /// Composition mode, first half: the DirectComposition device, a visual and a flip-model
    /// composition swap chain as its content. None of it needs the window, so this is the probe
    /// the window's style is chosen by (`WS_EX_NOREDIRECTIONBITMAP`, fixed at its creation): it
    /// fails where DirectComposition or `IDXGISwapChain2` is missing (RDP, VMs, old drivers).
    /// `transparent`: the canvas's alpha (premultiplied, as it is drawn) shows what is under the
    /// window (an overlay); else it is ignored.
    pub fn compose(&mut self, transparent: bool) -> windows::core::Result<()> {
        // SAFETY: DXGI and DirectComposition calls on this thread with our device; `d` is a live
        // local; the latency handle is kept with the swap chain that owns it.
        unsafe {
            let dxgi: IDXGIDevice = self.dev.cast()?;
            let device = dcomp_device(&dxgi)?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let (bw, bh) = (gpu::bucket(self.cw), gpu::bucket(self.ch));
            let d = DXGI_SWAP_CHAIN_DESC1 {
                Width: bw as u32,
                Height: bh as u32,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                // Sequential, not discard: a present copies only damage into the back buffer,
                // which must still hold the frame before the last.
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                // The only scaling a composition swap chain takes; `crop` undoes it.
                Scaling: DXGI_SCALING_STRETCH,
                AlphaMode: if transparent { DXGI_ALPHA_MODE_PREMULTIPLIED } else { DXGI_ALPHA_MODE_IGNORE },
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                ..Default::default()
            };
            let sc = factory.CreateSwapChainForComposition(&self.dev, &d, None)?;
            let sc2: IDXGISwapChain2 = sc.cast()?;
            sc2.SetMaximumFrameLatency(1)?;
            crop(&sc, self.cw, self.ch, bw, bh)?;
            let visual = device.CreateVisual()?;
            visual.SetContent(&sc)?;
            self.comp = Some(Composition { device, visual, target: None });
            self.adopt_swap(sc, &sc2, bw, bh);
            Ok(())
        }
    }

    /// Takes `sc` (`bw`×`bh` buffers) as the swap chain frames are presented to: its latency
    /// object owes the swap chain's start signal, and the first present is whole.
    fn adopt_swap(&mut self, sc: IDXGISwapChain1, sc2: &IDXGISwapChain2, bw: i32, bh: i32) {
        // SAFETY: the swap chain was just made on this thread; its latency handle is kept with it.
        self.latency = Some(unsafe { sc2.GetFrameLatencyWaitableObject() });
        self.owed = 1;
        self.swap = Some(sc);
        self.swap_wh = (bw, bh);
        self.first_present = true;
    }

    /// Composition mode, second half: shows the visual in `hwnd` (made with
    /// `WS_EX_NOREDIRECTIONBITMAP`), sized `w`x`h`.
    pub fn compose_into(&mut self, hwnd: HWND, w: i32, h: i32) -> windows::core::Result<()> {
        let Some(c) = self.comp.as_mut() else {
            return Err(failed("compose_into before compose".into()));
        };
        // SAFETY: DirectComposition calls on this thread with the caller's live window.
        unsafe {
            let target = c.device.CreateTargetForHwnd(hwnd, true)?;
            target.SetRoot(&c.visual)?;
            c.device.Commit()?;
            c.target = Some((target, hwnd));
        }
        Backend::resize(self, w, h);
        Ok(())
    }

    /// The buffer size of the composition swap chain for a `w`x`h` window. Buffers are kept
    /// while they fit and are not more than twice too large, else made in 256-px buckets.
    /// During a live resize (`immediate`) they never shrink, and when they must grow they grow
    /// to the monitor at once: a drag reallocates at most once. (Each reallocation made that
    /// step's present wait 5-7 ms on a 2560x1440 240 Hz laptop, holding a drag to ~60 steps/s.)
    /// `trim` takes them back to the window's bucket after.
    fn composition_size(&self, w: i32, h: i32) -> (i32, i32) {
        let (bw, bh) = self.swap_wh;
        let inside = w <= bw && h <= bh;
        if (inside && self.immediate) || gpu::fits((bw, bh), w, h) {
            (bw, bh)
        } else if self.immediate {
            let hwnd = self.comp.as_ref().and_then(|c| c.target.as_ref()).map(|t| t.1).unwrap_or_default();
            let (mw, mh) = monitor_size(hwnd);
            (gpu::bucket(w).max(mw).max(bw), gpu::bucket(h).max(mh).max(bh))
        } else {
            (gpu::bucket(w), gpu::bucket(h))
        }
    }

    /// Makes the swap chain's buffers `bw`x`bh` (when they are not) and shows the canvas's
    /// `cw`x`ch` of them.
    fn size_swap(&mut self, bw: i32, bh: i32) {
        let Some(sc) = self.swap.clone() else {
            return;
        };
        if (bw, bh) != self.swap_wh {
            // SAFETY: the immediate context through `&mut self`; unbinding first leaves no
            // reference to the buffers ResizeBuffers releases.
            unsafe { self.ctx.OMSetRenderTargets(None, None) };
            // SAFETY: our swap chain, no buffer of it bound or held (just unbound).
            let r = unsafe {
                sc.ResizeBuffers(
                    0,
                    bw as u32,
                    bh as u32,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
                )
            };
            self.check(r);
            self.swap_wh = (bw, bh);
            self.buffers_remade();
        }
        if self.comp.is_some() {
            let r = crop(&sc, self.cw, self.ch, self.swap_wh.0, self.swap_wh.1);
            self.check(r);
        }
        self.show_whole();
    }

    /// The swap chain's buffers were made again: the old ones are let go (flushed at the frame's
    /// end), the new ones hold nothing until the canvas is presented into them, and the latency
    /// object's count starts again. The presents queued before were discarded with the old
    /// buffers, and some never release the latency object; counting them would hold every frame
    /// that follows (the modal loop's gate waited for good: an animation froze while the border
    /// was held still). What has been released is consumed now; the count starts again from the
    /// next present.
    fn buffers_remade(&mut self) {
        self.released = true;
        self.unshown = true;
        self.drain_latency();
    }

    /// The next present shows the whole canvas, not damage.
    fn show_whole(&mut self) {
        self.first_present = true;
        self.prev_damage.clear();
    }

    /// Window mode: flip-model swap chain with a frame-latency waitable object.
    pub fn attach(&mut self, hwnd: HWND, w: i32, h: i32) -> windows::core::Result<()> {
        // SAFETY: DXGI calls on this thread with our device and the caller's live window; `d` is
        // a live local; the latency handle is kept with the swap chain that owns it.
        unsafe {
            let dxgi: IDXGIDevice = self.dev.cast()?;
            let adapter = dxgi.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let (bw, bh) = (w.max(1), h.max(1));
            let d = DXGI_SWAP_CHAIN_DESC1 {
                Width: bw as u32,
                Height: bh as u32,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                // While the border moves ahead of the next present, DWM shows the last frame
                // anchored top-left instead of stretched.
                Scaling: DXGI_SCALING_NONE,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                ..Default::default()
            };
            let sc = factory.CreateSwapChainForHwnd(&self.dev, hwnd, &d, None, None)?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            let sc2: IDXGISwapChain2 = sc.cast()?;
            sc2.SetMaximumFrameLatency(1)?;
            self.adopt_swap(sc, &sc2, bw, bh);
            Ok(())
        }
    }

    fn target_tex(&self, t: Target) -> Option<&Tex> {
        match t {
            Target::Canvas => Some(&self.canvas),
            Target::Layer(id) => self.layers.get(&id),
        }
    }

    fn srv(&self, t: TexRef) -> Option<ID3D11ShaderResourceView> {
        match t {
            TexRef::None => None,
            TexRef::Layer(id) => self.layers.get(&id).map(|t| t.srv.clone()),
            TexRef::Image(id) => self.images.get(&id).map(|t| t.srv.clone()),
            TexRef::ImageHdr(id) => self.hdr_images.get(&id).or_else(|| self.images.get(&id)).map(|t| t.srv.clone()),
        }
    }

    /// Blurs the target under one Backdrop instance and draws the material over it. Leaves the
    /// pipeline as `draw` set it (target, viewport, scissor, the instance buffer).
    fn backdrop(&mut self, target: Target, tw: i32, th: i32, clip: Rect, i: &Inst, all: &[Inst]) {
        let (rect, levels, extent) = gpu::backdrop_of(i);
        let region = backdrop_region(rect, extent, clip.intersect(&Rect::new(0, 0, tw, th)));
        if region.is_empty() {
            return;
        }
        let Some((src, rtv_target)) = self.target_tex(target).map(|t| (t.tex.clone(), t.rtv.clone())) else { return };
        let sizes = kawase_sizes(region.w, region.h, levels);
        // Grow the chain to fit.
        for (k, &(w, h)) in sizes.iter().enumerate() {
            // Same reuse rule as the canvas: a chain grown for a large window is let go when the
            // window shrinks to under half of it, instead of being held for good.
            let fits_k = self.blur.get(k).is_some_and(|t| fits(t, w, h));
            if !fits_k {
                if let Ok(t) = self.acquire_target(w, h) {
                    if k < self.blur.len() {
                        let old = std::mem::replace(&mut self.blur[k], t);
                        self.retire_target(old);
                    } else {
                        self.blur.push(t);
                    }
                } else {
                    return;
                }
            }
        }
        let ctx = self.ctx.clone();
        let b = box_of(region);
        // SAFETY: the immediate context, used only through `&mut self` on this thread; `b` lies
        // inside the target (`region` is clipped to it) and level 0 was grown to hold it.
        unsafe {
            ctx.OMSetRenderTargets(None, None);
            ctx.CopySubresourceRegion(&self.blur[0].tex, 0, 0, 0, 0, &src, 0, Some(&b));
        }
        // The passes: one full-level quad each, no blending.
        for (from, to, up) in kawase_passes(levels) {
            let (w, h) = sizes[to];
            let kind = if up { gpu::K_UP } else { gpu::K_DOWN };
            self.draw_one(&gpu::kawase_pass(&sizes, from, to), to, w, h, kind, from);
        }
        // Back to the target; the material reads level 0 at (p - region origin).
        let g = [tw as f32, th as f32, region.x as f32, region.y as f32];
        // SAFETY: the immediate context as above; `g` is the constant buffer's 16 bytes.
        unsafe {
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.OMSetRenderTargets(Some(&[rtv_target]), None);
            ctx.RSSetViewports(Some(&[viewport(tw, th)]));
            ctx.RSSetScissorRects(Some(&[rect_of(clip)]));
            ctx.UpdateSubresource(&self.cb, 0, None, g.as_ptr() as _, 0, 0);
        }
        if self.upload(std::slice::from_ref(i)) {
            // SAFETY: the immediate context as above; one instance was just uploaded.
            unsafe {
                ctx.OMSetBlendState(&self.blend_over, None, 0xffff_ffff);
                ctx.PSSetShader(self.builtin(gpu::K_BACKDROP), None);
                ctx.PSSetShaderResources(
                    1,
                    Some(&[Some(self.atlas.srv.clone()), Some(self.blur[0].srv.clone()), None]),
                );
                ctx.DrawInstanced(4, 1, 0, 0);
            }
        }
        // Restore `draw`'s state: its constants and its instances.
        let g = [tw as f32, th as f32, 0.0, 0.0];
        // SAFETY: the immediate context as above; `g` is the constant buffer's 16 bytes.
        unsafe {
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.UpdateSubresource(&self.cb, 0, None, g.as_ptr() as _, 0, 0);
        }
        self.upload(all);
    }

    /// Draws one instance into blur level `to` (viewport w×h) reading level `from`.
    fn draw_one(&mut self, inst: &Inst, to: usize, w: i32, h: i32, kind: u8, from: usize) {
        let ctx = self.ctx.clone();
        let g = [w as f32, h as f32, 0.0, 0.0];
        // SAFETY: the immediate context, used only through `&mut self` on this thread; `g` is
        // the constant buffer's 16 bytes.
        unsafe {
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.OMSetRenderTargets(Some(&[self.blur[to].rtv.clone()]), None);
            ctx.UpdateSubresource(&self.cb, 0, None, g.as_ptr() as _, 0, 0);
            ctx.RSSetViewports(Some(&[viewport(w, h)]));
            ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: w, bottom: h }]));
        }
        if !self.upload(std::slice::from_ref(inst)) {
            return;
        }
        // SAFETY: the immediate context as above; one instance was just uploaded.
        unsafe {
            ctx.OMSetBlendState(gpu::blends(kind).then_some(&self.blend_over), None, 0xffff_ffff);
            ctx.PSSetShader(self.builtin(kind), None);
            ctx.PSSetShaderResources(1, Some(&[Some(self.atlas.srv.clone()), Some(self.blur[from].srv.clone()), None]));
            ctx.DrawInstanced(4, 1, 0, 0);
        }
    }

    /// Writes `insts` to the instance buffer, growing it (and binding the new one) first when
    /// they do not fit. `false` when they could not be written: none of them may be drawn.
    fn upload(&mut self, insts: &[Inst]) -> bool {
        if insts.len() > self.vb_cap {
            let cap = insts.len().next_power_of_two();
            let Ok(vb) = Self::vbuf(&self.dev, cap) else { return false };
            self.vb = vb;
            self.vb_cap = cap;
            let stride = INST_SIZE as u32;
            let offset = 0u32;
            // SAFETY: the immediate context, used only through `&mut self` on this thread;
            // stride and offset are live locals.
            unsafe { self.ctx.IASetVertexBuffers(0, 1, Some(&Some(self.vb.clone())), Some(&stride), Some(&offset)) };
        }
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the buffer holds `vb_cap` >= `insts.len()` instances (`vb_cap` is set only
        // together with the buffer that has it); the mapping is written, then unmapped, while
        // no draw reads it.
        unsafe {
            if self.ctx.Map(&self.vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_err() {
                return false;
            }
            std::ptr::copy_nonoverlapping(insts.as_ptr() as *const u8, m.pData as *mut u8, insts.len() * INST_SIZE);
            self.ctx.Unmap(&self.vb, 0);
        }
        true
    }

    /// A built-in program's shader: `new` fails unless every one of them compiled.
    fn builtin(&self, kind: u8) -> &ID3D11PixelShader {
        self.ps[kind as usize].as_ref().expect("built-in shaders are compiled by `new`")
    }

    fn check(&mut self, r: windows::core::Result<()>) {
        if let Err(e) = r {
            let c = e.code();
            if c == DXGI_ERROR_DEVICE_REMOVED || c == DXGI_ERROR_DEVICE_RESET || c == DXGI_ERROR_DEVICE_HUNG {
                self.lost = true;
            }
        }
    }
}

impl Backend for D3d11 {
    fn name(&self) -> &'static str {
        "d3d11"
    }
    fn resize(&mut self, w: i32, h: i32) {
        if !fits(&self.canvas, w, h) {
            if let Ok(t) = self.acquire_target(w, h) {
                let old = std::mem::replace(&mut self.canvas, t);
                self.retire_target(old);
            }
        }
        self.cw = w;
        self.ch = h;
        // A window swap chain's buffers are exactly the window: DWM stretches a smaller source
        // region (`SetSourceSize`) over them under every scaling mode, `DXGI_SCALING_NONE`
        // included. A composition swap chain's are larger and cropped (`composition_size`). The
        // memory a long drag used to pile up is held by flushing after each reallocation and by
        // gating the modal loop's frames on the last present (win32.rs).
        let (bw, bh) = if self.comp.is_some() { self.composition_size(w, h) } else { (w.max(1), h.max(1)) };
        self.size_swap(bw, bh);
    }
    fn atlas_size(&self) -> i32 {
        ATLAS
    }
    fn upload_atlas(&mut self, r: Rect, atlas: &[u8], aw: i32) {
        let b = box_of(r);
        // The update reads `r.h` rows of `aw` pixels from `r`'s first one: `atlas` must hold them.
        if !atlas_rows_fit(r, aw, atlas.len()) {
            return;
        }
        let first = (r.y * aw + r.x) as usize;
        // SAFETY: `r`'s rows lie inside `atlas` (checked above); the update copies them before
        // returning, on the immediate context through `&mut self`.
        unsafe {
            let p = atlas.as_ptr().add(first);
            self.ctx.UpdateSubresource(&self.atlas.tex, 0, Some(&b), p as _, aw as u32, 0);
        }
    }
    fn layer_alloc(&mut self, id: u32, w: i32, h: i32) {
        if self.layers.get(&id).is_some_and(|t| fits(t, w, h)) {
            return;
        }
        // Allocation invalidates contents. The tree schedules one full logical-layer draw,
        // whose first operation clears it; clearing the entire bucket here duplicates that work.
        if let Ok(t) = self.acquire_target(w, h) {
            if let Some(old) = self.layers.insert(id, t) {
                self.retire_target(old);
            }
        }
    }
    fn layer_free(&mut self, id: u32) {
        if let Some(old) = self.layers.remove(&id) {
            self.retire_target(old);
        }
    }
    fn image_upload(&mut self, id: u32, w: i32, h: i32, rgba: &[u8]) {
        if let Ok(t) = Self::image_tex(&self.dev, w, h, rgba) {
            if let Some(old) = self.images.insert(id, t) {
                self.retire_target(old);
            }
        }
    }
    fn image_make(&mut self, img: &crate::image::ImageHandle, wake: Option<&Wake>) {
        let (id, w, h) = (img.id(), img.width(), img.height());
        if let Some(wake) = wake.filter(|_| make_apart(w, h)) {
            if self.maker.is_none() {
                self.maker = Maker::start(&self.dev);
            }
            let headroom = self.hdr.map(|o| o.headroom()).filter(|_| img.has_gain_map());
            if let Some(m) = &mut self.maker {
                if m.jobs.send((img.clone(), wake.clone(), headroom)).is_ok() {
                    m.making.insert(id);
                    return;
                }
            }
        }
        self.image_upload(id, w, h, img.pixels());
        if let Some(t) = self.hdr.and_then(|o| hdr_tex(&self.dev, img, o.headroom())) {
            self.hdr_images.insert(id, t);
        }
    }
    fn images_made(&mut self, made: &mut Vec<u32>) {
        let Some(m) = &mut self.maker else { return };
        while let Ok((id, t)) = m.done.try_recv() {
            if !m.making.remove(&id) {
                continue;
            }
            if let Some(Made(t, hdr)) = t {
                // `retire_target`'s, written out: `m` holds the maker.
                if self.images.insert(id, t).is_some() {
                    self.released = true;
                }
                // Made for an output that is still HDR (a switch since has every image made
                // again: `set_output`).
                match hdr.filter(|_| self.hdr.is_some()) {
                    Some(h) => {
                        self.hdr_images.insert(id, h);
                    }
                    None => {
                        self.hdr_images.remove(&id);
                    }
                }
                made.push(id);
            }
        }
    }
    fn image_free(&mut self, id: u32) {
        if let Some(m) = &mut self.maker {
            m.making.remove(&id);
        }
        if let Some(old) = self.images.remove(&id) {
            self.retire_target(old);
        }
        self.hdr_images.remove(&id);
    }
    fn set_output(&mut self, hdr: Option<HdrOutput>) -> bool {
        if hdr == self.hdr {
            return false;
        }
        let mut hdr = hdr;
        let half = |h: &Option<HdrOutput>| {
            if h.is_some() {
                DXGI_FORMAT_R16G16B16A16_FLOAT
            } else {
                DXGI_FORMAT_R8G8B8A8_UNORM
            }
        };
        if half(&hdr) != self.fmt && !self.swap_format(hdr.is_some()) && hdr.is_some() {
            // The swap chain cannot show scRGB here: SDR, as it was.
            eprintln!("novaui[d3d11]: the swap chain cannot show scRGB; the output is drawn as SDR");
            self.swap_format(false);
            hdr = None;
        }
        if hdr == self.hdr {
            return false;
        }
        let fmt = half(&hdr);
        if fmt != self.fmt {
            self.fmt = fmt;
            // Targets in the new format; the tree allocates and draws them all again.
            if let Ok(t) = self.acquire_target(self.cw, self.ch) {
                let old = std::mem::replace(&mut self.canvas, t);
                self.retire_target(old);
            }
            for (_, t) in std::mem::take(&mut self.layers) {
                self.retire_target(t);
            }
            for t in std::mem::take(&mut self.blur) {
                self.retire_target(t);
            }
        }
        self.hdr = hdr;
        // Renditions are made for one headroom: the tree uploads every image again.
        self.hdr_images.clear();
        if let Some(m) = &mut self.maker {
            m.making.clear();
        }
        self.first_present = true;
        true
    }
    fn end_frame(&mut self) {
        if std::mem::take(&mut self.unshown) {
            // New buffers hold nothing yet; the canvas holds the whole last frame.
            self.present(&[]);
        }
        if std::mem::take(&mut self.released) {
            // SAFETY: the immediate context through `&mut self`, on this thread.
            unsafe { self.ctx.Flush() };
        }
    }
    fn trim(&mut self) {
        // Released render targets stay in the driver's pools until it is told to let them go:
        // after a resize that ends smaller, the memory of the larger size is otherwise kept.
        self.release_pools();
        // A live resize grew the composition buffers to the monitor; the window has been still
        // for a while (a trim follows the last resize), so they go back to its bucket.
        if self.comp.is_some() && !self.immediate {
            let (bw, bh) = self.composition_size(self.cw, self.ch);
            self.size_swap(bw, bh);
        }
    }
    fn release_pools(&mut self) {
        // SAFETY: the immediate context through `&mut self`, on this thread; `draw` sets every
        // state it uses again, so clearing it loses nothing.
        unsafe {
            self.ctx.ClearState();
            self.ctx.Flush();
            if let Ok(d) = self.dev.cast::<IDXGIDevice3>() {
                d.Trim();
            }
        }
    }
    fn draw(&mut self, target: Target, clip: Rect, off: (i32, i32), clear: Color, draws: &[Draw]) {
        // A program that does not compile here has its draws skipped (no shader); the rest of the
        // frame is drawn.
        for (_, p) in crate::image::effect::added_since(&mut self.effects) {
            let shader = compile(p.gpu.hlsl, gpu::EFFECT_ENTRY, "ps_5_0").and_then(|b| {
                let mut p = None;
                // SAFETY: `b` is compiled bytecode; `p` a live out-parameter.
                unsafe { self.dev.CreatePixelShader(&b, None, Some(&mut p))? };
                made(p)
            });
            if let Err(err) = &shader {
                eprintln!("novaui[d3d11] image effect {}: {err}", p.name);
            }
            self.ps.push(shader.ok());
        }
        let Some(tt) = self.target_tex(target) else { return };
        let (tw, th, tt_rtv) = (tt.w, tt.h, tt.rtv.clone());
        let clip = clip.intersect(&Rect::new(0, 0, tw, th));
        if clip.is_empty() {
            return;
        }
        let clear = target.clear(clear);
        let mut insts = std::mem::take(&mut self.insts);
        let mut batches = std::mem::take(&mut self.batches);
        gpu::encode(clear, clip, off, draws, &mut insts, &mut batches);
        if !self.upload(&insts) {
            self.insts = insts;
            self.batches = batches;
            return;
        }
        // SAFETY: the immediate context, used only through `&mut self` on this thread; the
        // instance buffer holds `insts` (uploaded above) and every batch indexes into them;
        // `size` is the constant buffer's 16 bytes.
        unsafe {
            let size = [tw as f32, th as f32, 0.0, 0.0];
            self.ctx.UpdateSubresource(&self.cb, 0, None, size.as_ptr() as _, 0, 0);
            let ctx = self.ctx.clone();
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            let rtv = tt_rtv.clone();
            ctx.OMSetRenderTargets(Some(&[rtv]), None);
            ctx.RSSetViewports(Some(&[viewport(tw, th)]));
            ctx.RSSetScissorRects(Some(&[rect_of(clip)]));
            self.bind_quads(&ctx);
            for b in &batches {
                if b.kind == gpu::K_BACKDROP {
                    let i = insts[b.first as usize];
                    self.backdrop(target, tw, th, clip, &i, &insts);

                    continue;
                }
                let blend = gpu::blends(b.kind).then_some(&self.blend_over);
                let Some(ps) = self.ps[b.kind as usize].as_ref() else { continue };
                let (a, bb) = (self.srv(b.tex.0), self.srv(b.tex.1));
                if !gpu::drawable(b.tex, |t| self.srv(t).is_some()) {
                    continue;
                }
                ctx.OMSetBlendState(blend, None, 0xffff_ffff);
                ctx.PSSetShader(ps, None);
                ctx.PSSetShaderResources(1, Some(&[Some(self.atlas.srv.clone()), a, bb]));
                ctx.DrawInstanced(4, b.count, 0, b.first);
            }
            ctx.PSSetShaderResources(1, Some(&[None, None, None]));
            ctx.OMSetRenderTargets(None, None);
        }
        self.insts = insts;
        self.batches = batches;
    }
    fn present(&mut self, damage: &[Rect]) {
        let Some(sc) = self.swap.clone() else {
            return;
        };
        // SAFETY: our swap chain and immediate context through `&mut self`, on this thread; each
        // copy box lies inside both the canvas and the back buffer (damage is clipped to the
        // window); `dd` outlives Present1, which reads `DirtyRectsCount` rectangles from it.
        unsafe {
            let bb: ID3D11Texture2D = match sc.GetBuffer(0) {
                Ok(b) => b,
                Err(e) => {
                    self.check(Err(e));
                    return;
                }
            };
            let full = [Rect::new(0, 0, self.cw, self.ch)];
            let mut both = Damage::default();
            for r in damage.iter().chain(self.prev_damage.iter()) {
                both.add(*r);
            }
            let both = both.take();
            let whole = self.first_present;
            let rects: &[Rect] = if whole { &full } else { &both };
            if let Some(o) = self.hdr {
                self.present_hdr(&bb, rects, o.sdr_white / 80.0);
            }
            for r in rects.iter().filter(|_| self.hdr.is_none()) {
                self.ctx.CopySubresourceRegion(
                    &bb,
                    0,
                    r.x as u32,
                    r.y as u32,
                    0,
                    &self.canvas.tex,
                    0,
                    Some(&box_of(*r)),
                );
            }
            let dd: Vec<RECT> = damage.iter().map(|r| rect_of(*r)).collect();
            let params = DXGI_PRESENT_PARAMETERS {
                DirtyRectsCount: if whole { 0 } else { dd.len() as u32 },
                pDirtyRects: dd.as_ptr() as *mut _,
                ..Default::default()
            };
            // A live move / size presents at once (vsync there held the border back a frame).
            let hr = sc.Present1(if self.immediate { 0 } else { 1 }, DXGI_PRESENT(0), &params);
            self.check(hr.ok());
            self.prev_damage = if whole { full.to_vec() } else { damage.to_vec() };
            self.first_present = false;
            self.unshown = false;
            if std::mem::take(&mut self.released) {
                self.ctx.Flush();
            }
            self.owed = self.owed.saturating_add(1);
            self.presented_at = std::time::Instant::now();
        }
    }
    /// Consumes every owed signal: the ones of presents that already retired return at once,
    /// the last waits until the newest present retires (the queue has room).
    fn wait_frame(&mut self) {
        let Some(h) = self.latency else { return };
        while self.owed > 0 {
            // Bounded: a throttled compositor (resize, occlusion) must not stall input. A wait
            // that times out gives up on the rest (a signal that will not come).
            // SAFETY: the swap chain's own latency handle, alive with `self.swap`.
            let r = unsafe { windows::Win32::System::Threading::WaitForSingleObjectEx(h, 34, true) };
            self.owed -= 1;
            if r != windows::Win32::Foundation::WAIT_OBJECT_0 {
                self.owed = 0;
            }
        }
    }
    /// Consumes the owed signals that are already there, without blocking; ready when none is
    /// left. A signal is a hint, never a condition that can hold frames for good: one still
    /// missing 34 ms after the newest present (the bound `wait_frame` gives up at) is taken as
    /// lost, and the frame goes.
    fn frame_ready(&mut self) -> bool {
        let Some(h) = self.latency else { return true };
        while self.owed > 0 {
            // SAFETY: the swap chain's own latency handle, alive with `self.swap`.
            let r = unsafe { windows::Win32::System::Threading::WaitForSingleObjectEx(h, 0, false) };
            if r != windows::Win32::Foundation::WAIT_OBJECT_0 {
                if self.presented_at.elapsed() >= std::time::Duration::from_millis(34) {
                    self.owed = 0;
                    return true;
                }
                return false;
            }
            self.owed -= 1;
        }
        true
    }
    fn adapter(&self) -> String {
        self.adapter.clone()
    }
    #[cfg(test)]
    fn resources(&self) -> Option<BackendResources> {
        let px = if self.hdr.is_some() { 8 } else { 4 };
        let rgba_bytes = |t: &Tex| t.w as u64 * t.h as u64 * px;
        let atlas_bytes = self.atlas.w as u64 * self.atlas.h as u64;
        let canvas_bytes = rgba_bytes(&self.canvas);
        let layer_bytes = self.layers.values().map(rgba_bytes).sum::<u64>();
        let image_bytes = self.images.values().map(rgba_bytes).sum::<u64>();
        let scratch_bytes = self.blur.iter().map(rgba_bytes).sum::<u64>();
        let texture_bytes = atlas_bytes + canvas_bytes + layer_bytes + image_bytes + scratch_bytes;
        Some(BackendResources { texture_bytes, canvas_bytes, atlas_bytes, layer_bytes, image_bytes, scratch_bytes })
    }
    fn set_immediate(&mut self, on: bool) {
        self.immediate = on;
    }
    fn lost(&self) -> bool {
        self.lost
    }
    fn finish(&mut self) {
        use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
        // SAFETY: our device and immediate context through `&mut self`, on this thread; the
        // fence, its context and event are this device's and live in `self.completion` across
        // the wait; GetData writes the event query's 4-byte BOOL into `done`.
        unsafe {
            if self.completion.is_none() {
                let fence = (|| {
                    let dev = self.dev.cast::<ID3D11Device5>().ok()?;
                    let ctx = self.ctx.cast::<ID3D11DeviceContext4>().ok()?;
                    let mut fence: Option<ID3D11Fence> = None;
                    dev.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence).ok()?;
                    let event = Event(CreateEventW(None, false, false, None).ok()?);
                    Some(Completion::Fence { fence: fence?, ctx, event, value: 0 })
                })();
                self.completion = match fence {
                    Some(f) => Some(f),
                    None => {
                        let d = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
                        let mut q = None;
                        if self.dev.CreateQuery(&d, Some(&mut q)).is_err() {
                            self.lost = true;
                            return;
                        }
                        q.map(Completion::Query)
                    }
                };
            }
            match &mut self.completion {
                None => {}
                Some(Completion::Fence { fence, ctx, event, value }) => {
                    *value += 1;
                    let v = *value;
                    if ctx.Signal(&*fence, v).is_err() {
                        self.lost = true;
                        return;
                    }
                    self.ctx.Flush();
                    if fence.GetCompletedValue() >= v {
                        return;
                    }
                    if fence.SetEventOnCompletion(v, event.0).is_err() {
                        self.lost = true;
                        return;
                    }
                    // Sleeps until the GPU reaches the signal. A removed device completes its
                    // fences, which sets the event too; the timeout only rechecks.
                    loop {
                        let w = WaitForSingleObject(event.0, 1000);
                        if w == WAIT_OBJECT_0 || fence.GetCompletedValue() >= v {
                            break;
                        }
                        if w != WAIT_TIMEOUT || self.dev.GetDeviceRemovedReason().is_err() {
                            self.lost = true;
                            break;
                        }
                    }
                }
                Some(Completion::Query(q)) => {
                    // This path waits to completion, so the query can be reused on the next call.
                    let q = q.clone();
                    self.ctx.End(&q);
                    self.ctx.Flush();
                    let mut done = 0i32;
                    loop {
                        if let Err(e) = self.ctx.GetData(&q, Some(&mut done as *mut _ as _), 4, 0) {
                            self.check(Err(e));
                            return;
                        }
                        if done != 0 {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_micros(250));
                    }
                }
            }
        }
    }
    fn readback(&mut self) -> Vec<u32> {
        let (w, h) = (self.cw, self.ch);
        // SAFETY: our device and immediate context through `&mut self`, on this thread; the
        // staging texture is w×h RGBA8, mapped for reading: each row read is `w * 4` bytes at
        // its `RowPitch` offset, inside the mapping, and read before Unmap.
        unsafe {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w as u32,
                Height: h as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: self.fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut s = None;
            let s: ID3D11Texture2D = match self.dev.CreateTexture2D(&d, None, Some(&mut s)).and_then(|()| made(s)) {
                Ok(s) => s,
                Err(e) => {
                    self.check(Err(e));
                    return Vec::new();
                }
            };
            let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: w as u32, bottom: h as u32, back: 1 };
            self.ctx.CopySubresourceRegion(&s, 0, 0, 0, 0, &self.canvas.tex, 0, Some(&bx));
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if let Err(e) = self.ctx.Map(&s, 0, D3D11_MAP_READ, 0, Some(&mut m)) {
                self.check(Err(e));
                return Vec::new();
            }
            let mut out = Vec::with_capacity((w * h) as usize);
            let half = self.fmt == DXGI_FORMAT_R16G16B16A16_FLOAT;
            // Half floats (an HDR canvas) read as the 8-bit values they encode, clamped to 1.
            let byte = |v: u16| (crate::image::gainmap::f32_of(v).clamp(0.0, 1.0) * 255.0).round() as u32;
            for y in 0..h {
                let row = std::slice::from_raw_parts(
                    (m.pData as *const u8).add(y as usize * m.RowPitch as usize),
                    w as usize * if half { 8 } else { 4 },
                );
                if half {
                    for p in row.chunks(8) {
                        let c = |k: usize| byte(u16::from_le_bytes([p[2 * k], p[2 * k + 1]]));
                        out.push(c(3) << 24 | c(0) << 16 | c(1) << 8 | c(2));
                    }
                    continue;
                }
                for p in row.chunks(4) {
                    out.push(argb_of_rgba8(p));
                }
            }
            self.ctx.Unmap(&s, 0);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::gainmap::tests::{jpeg, ultra_hdr, XMP};

    /// An image drawn with `hdr` takes its HDR rendition on an HDR output only; without `hdr`, and
    /// on SDR, it is the picture as it is.
    #[test]
    fn hdr_images_are_brighter_only_where_asked_and_shown() {
        let Ok(mut gpu) = D3d11::new(48, 16) else {
            eprintln!("no D3D11 device; skipped");
            return;
        };
        let file = ultra_hdr(
            &jpeg(16, 16, |_, _| image::Rgb([150, 150, 150]), false),
            &jpeg(8, 8, |_, _| image::Rgb([255, 255, 255]), true),
            XMP,
        );
        let img = crate::image::decode(&file, crate::image::Fit::Within(16)).unwrap();
        let at = |x| Prim::Image {
            rect: Rect::new(x, 0, 16, 16),
            image: img.id(),
            uv: [0.0, 0.0, 1.0, 1.0],
            radius: 0.0,
            opacity: 1.0,
            hdr: x > 0,
        };
        let draws = [0, 32].map(|x| Draw { prim: at(x), clip: Rect::new(0, 0, 48, 16) });
        let frame = |gpu: &mut D3d11| {
            gpu.image_make(&img, None);
            gpu.draw(Target::Canvas, Rect::new(0, 0, 48, 16), (0, 0), [0.0, 0.0, 0.0, 1.0], &draws);
            let px = gpu.readback();
            (px[8 * 48 + 8] & 0xFF, px[8 * 48 + 40] & 0xFF)
        };
        let (plain, asked) = frame(&mut gpu);
        assert!(plain.abs_diff(150) <= 2 && asked == plain, "SDR: {plain} {asked}");
        assert!(gpu.set_output(Some(HdrOutput { sdr_white: 200.0, peak: 800.0 })));
        let (plain_hdr, asked_hdr) = frame(&mut gpu);
        // The plain one is the same colour; the asked one is raised past SDR white (read back
        // clamped to it).
        assert!(plain_hdr.abs_diff(plain) <= 1 && asked_hdr == 255, "HDR: {plain_hdr} {asked_hdr}");
    }
}
