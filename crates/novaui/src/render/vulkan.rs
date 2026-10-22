//! Vulkan backend. Same WGSL source (SPIR-V compiled when novaui is built), same instance
//! encoding and batches as D3D11, same resource semantics: render targets in 256-px buckets
//! reused while they fit, immutable images shared by alias, a dual-Kawase chain for the
//! material, damage-only presentation.
//!
//! Synchronisation model: every image stays in GENERAL layout; a full memory barrier follows each
//! render pass or transfer so the next one may sample what the previous wrote. A frame is one
//! command buffer and one submit. At most one frame is in flight: recording the next frame first
//! waits for the previous submit's fence, which is also when the per-frame rings (instances,
//! uniforms, descriptor sets, staging) are reset and retired resources destroyed.
//!
//! The contract of every `unsafe fn` here, and what each `unsafe` block below relies on unless
//! its comment says more: `Vulkan` is used through `&mut self` (or `&self`), so from one thread at
//! a time and never concurrently; `dev` and `inst` live until `Drop`, which destroys everything
//! else first and waits for the device to go idle; every handle passed was created by this
//! device and not yet destroyed (retired resources go to `dead` and are destroyed only once the
//! fence says the GPU is done with them); commands are recorded only between `begin` and
//! `submit`; and every create-info is a live local for the call.

use crate::geom::*;
use crate::render::batch::{self as gpu, Batch, Inst, Tex as TexRef, INST_SIZE};
use crate::render::primitive::*;
use ash::vk;
use std::collections::HashMap;

const FMT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const UBO_SLOT: usize = 256;
const UBO_SLOTS: usize = 4096;
const MAX_SETS: u32 = 4096;
const STAGING: usize = 8 << 20;
/// Ring entries one material uses: 2 per Kawase level (at most 6) plus its own draw.
const BACKDROP_ROOM: usize = 13;

struct Img {
    img: vk::Image,
    mem: vk::DeviceMemory,
    view: vk::ImageView,
    fb: vk::Framebuffer,
    w: i32,
    h: i32,
}

struct Buf {
    buf: vk::Buffer,
    mem: vk::DeviceMemory,
    ptr: *mut u8,
    cap: usize,
}

/// Textures of large images, made on a thread of their own as D3D11's are: there the image, its
/// memory and a staging buffer holding its pixels are made (the copy of megabytes and the
/// allocations), and the UI thread only records the transfer when it takes one in
/// (`images_made`). Creating, allocating and mapping need external synchronisation of nothing
/// but the objects they make, which are the thread's alone until it sends them.
struct Maker {
    /// `None` once the backend is going (`Drop`): the thread ends.
    jobs: Option<std::sync::mpsc::Sender<(crate::image::ImageHandle, Wake)>>,
    done: std::sync::mpsc::Receiver<(u32, Option<Made>)>,
    /// Asked for and not taken in yet. An image freed meanwhile leaves the set; what comes for
    /// it is destroyed.
    making: std::collections::HashSet<u32>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// An image the maker made, and the buffer its pixels wait in.
struct Made {
    img: Img,
    staging: Buf,
}
// SAFETY: plain Vulkan handles and a mapping of the staging buffer's own memory, owned by the one
// `Made`: whichever thread holds it is the only one that uses them.
unsafe impl Send for Made {}

impl Maker {
    fn start(dev: &ash::Device, mem: vk::PhysicalDeviceMemoryProperties) -> Option<Maker> {
        let (jobs, rx) = std::sync::mpsc::channel::<(crate::image::ImageHandle, Wake)>();
        let (tx, done) = std::sync::mpsc::channel();
        let dev = dev.clone();
        let thread = std::thread::Builder::new()
            .name("novaui-images".into())
            .spawn(move || {
                // Ends when the backend drops the sender; `Drop` joins it before the device goes.
                for (img, wake) in rx {
                    let (w, h, px) = (img.width(), img.height(), img.pixels());
                    // SAFETY: the device outlives this thread (`Drop` joins it first); everything
                    // made here is this thread's until sent; the copy writes at most the
                    // buffer's `n` bytes, read from `px`, which holds at least as many.
                    let made = unsafe {
                        let n = (w.max(1) * h.max(1) * 4) as usize;
                        new_image(&dev, &mem, w, h, FMT, false).ok().and_then(|img| {
                            match new_buffer(&dev, &mem, n, vk::BufferUsageFlags::TRANSFER_SRC) {
                                Ok(staging) => {
                                    std::ptr::copy_nonoverlapping(px.as_ptr(), staging.ptr, n.min(px.len()));
                                    Some(Made { img, staging })
                                }
                                Err(_) => {
                                    destroy_image(&dev, &img);
                                    None
                                }
                            }
                        })
                    };
                    if tx.send((img.id(), made)).is_err() {
                        return;
                    }
                    wake();
                }
            })
            .ok()?;
        Some(Maker { jobs: Some(jobs), done, making: Default::default(), thread: Some(thread) })
    }
}

fn mem_type(props: &vk::PhysicalDeviceMemoryProperties, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
    (0..props.memory_type_count)
        .find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags))
}

/// A host-visible, mapped buffer.
unsafe fn new_buffer(
    dev: &ash::Device,
    props: &vk::PhysicalDeviceMemoryProperties,
    size: usize,
    usage: vk::BufferUsageFlags,
) -> Result<Buf, String> {
    let buf = dev
        .create_buffer(&vk::BufferCreateInfo::default().size(size as u64).usage(usage), None)
        .map_err(|e| format!("{e}"))?;
    let req = dev.get_buffer_memory_requirements(buf);
    let Some(mt) = mem_type(
        props,
        req.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ) else {
        dev.destroy_buffer(buf, None);
        return Err("no host-visible memory".into());
    };
    let mem = match dev
        .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mt), None)
    {
        Ok(m) => m,
        Err(e) => {
            dev.destroy_buffer(buf, None);
            return Err(format!("{e}"));
        }
    };
    let b = Buf { buf, mem, ptr: std::ptr::null_mut(), cap: size };
    if let Err(e) = dev.bind_buffer_memory(buf, mem, 0) {
        free_buffer(dev, &b);
        return Err(format!("{e}"));
    }
    match dev.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) {
        Ok(p) => Ok(Buf { ptr: p as *mut u8, ..b }),
        Err(e) => {
            free_buffer(dev, &b);
            Err(format!("{e}"))
        }
    }
}

unsafe fn free_buffer(dev: &ash::Device, b: &Buf) {
    if b.buf != vk::Buffer::null() {
        dev.destroy_buffer(b.buf, None);
        dev.free_memory(b.mem, None);
    }
}

/// A device-local image with its view, in UNDEFINED layout (no framebuffer, no commands): what
/// any thread can make.
unsafe fn new_image(
    dev: &ash::Device,
    props: &vk::PhysicalDeviceMemoryProperties,
    w: i32,
    h: i32,
    fmt: vk::Format,
    rt: bool,
) -> Result<Img, String> {
    let mut usage =
        vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC;
    if rt {
        usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
    }
    let (w, h) = (w.max(1), h.max(1));
    let ci = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(fmt)
        .extent(vk::Extent3D { width: w as u32, height: h as u32, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let img = dev.create_image(&ci, None).map_err(|e| format!("image {w}x{h}: {e}"))?;
    let req = dev.get_image_memory_requirements(img);
    let Some(mt) = mem_type(props, req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
        dev.destroy_image(img, None);
        return Err("no device memory".into());
    };
    let mem = match dev
        .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mt), None)
    {
        Ok(m) => m,
        Err(e) => {
            dev.destroy_image(img, None);
            return Err(format!("image memory: {e}"));
        }
    };
    let fail = |e: vk::Result| {
        dev.destroy_image(img, None);
        dev.free_memory(mem, None);
        format!("{e}")
    };
    dev.bind_image_memory(img, mem, 0).map_err(fail)?;
    let view = dev
        .create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(fmt)
                .subresource_range(RANGE),
            None,
        )
        .map_err(fail)?;
    Ok(Img { img, mem, view, fb: vk::Framebuffer::null(), w, h })
}

unsafe fn destroy_image(dev: &ash::Device, i: &Img) {
    if i.fb != vk::Framebuffer::null() {
        dev.destroy_framebuffer(i.fb, None);
    }
    if i.view != vk::ImageView::null() {
        dev.destroy_image_view(i.view, None);
        dev.destroy_image(i.img, None);
        dev.free_memory(i.mem, None);
    }
}

/// A window surface as the host hands it over.
pub enum Window {
    #[cfg(target_os = "linux")]
    Xlib { display: *mut std::ffi::c_void, window: u64 },
    #[cfg(target_os = "linux")]
    Wayland { display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void },
    #[cfg(windows)]
    Win32 { hinstance: isize, hwnd: isize },
}

struct Wsi {
    surface_i: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    sc_d: ash::khr::swapchain::Device,
    sc: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    /// Damage image i has not yet received; `None`: never written, needs a full copy.
    pending: Vec<Option<Damage>>,
    acquire: vk::Semaphore,
    /// One per swap chain image: a present may still wait on image i's semaphore.
    done: Vec<vk::Semaphore>,
    format: vk::Format,
    extent: (i32, i32),
    stale: bool,
}

/// The loader stays mapped for the life of the process: drivers keep threads that run loader code
/// after the last device is destroyed (unloading it crashed the NVIDIA driver on teardown).
fn entry() -> Result<&'static ash::Entry, String> {
    static ENTRY: std::sync::OnceLock<Result<ash::Entry, String>> = std::sync::OnceLock::new();
    ENTRY
        // SAFETY: loads the system's Vulkan loader once, kept for the life of the process (see
        // above), so no function it hands out outlives it.
        .get_or_init(|| unsafe { ash::Entry::load() }.map_err(|e| format!("vulkan loader: {e}")))
        .as_ref()
        .map_err(|e| e.clone())
}

pub struct Vulkan {
    _entry: &'static ash::Entry,
    inst: ash::Instance,
    pdev: vk::PhysicalDevice,
    dev: ash::Device,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    qf: u32,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    in_flight: bool,
    recording: bool,
    rp: vk::RenderPass,
    dsl: vk::DescriptorSetLayout,
    pl: vk::PipelineLayout,
    pipes: Vec<vk::Pipeline>,
    /// Image effects the pipelines cover (`image::effect`); more registered later are added at
    /// the next draw.
    effects: usize,
    dpool: vk::DescriptorPool,
    sets_used: u32,
    ubo: Buf,
    ubo_slot: usize,
    insts_buf: Buf,
    inst_used: usize,
    staging: Buf,
    staging_used: usize,
    dead_bufs: Vec<Buf>,
    dead: Vec<Img>,
    atlas: Img,
    dummy: Img,
    canvas: Img,
    cw: i32,
    ch: i32,
    layers: HashMap<u32, Img>,
    images: HashMap<u32, Img>,
    /// Makes large images' textures off the UI thread; started by the first.
    maker: Option<Maker>,
    blur: Vec<Img>,
    insts: Vec<Inst>,
    batches: Vec<Batch>,
    wsi: Option<Wsi>,
    has_surface_ext: bool,
    adapter: String,
    lost: bool,
}

/// Same policy as D3D11: 256-px buckets, reused while not more than twice too large.
/// The presentation surface's size for a `want`-sized window when it is now `cur`: it only
/// changes when the window outgrows it or becomes much smaller, in 256-px steps, so a live
/// resize rebuilds the swap chain (8-10 ms on NVIDIA/X11) once per step instead of every frame.
/// The X11 host keeps a child window of this size; the canvas is copied into its top-left.
pub fn surface_fit(cur: (i32, i32), want: (i32, i32)) -> (i32, i32) {
    let axis = |c: i32, w: i32| if c >= w && c <= bucket(w) + 256 { c } else { bucket(w) };
    (axis(cur.0, want.0), axis(cur.1, want.1))
}

use gpu::bucket;

/// Whether `i` is reused for a `w`×`h` target (`batch::fits`).
fn fits(i: &Img, w: i32, h: i32) -> bool {
    gpu::fits((i.w, i.h), w, h)
}

const RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};
const LAYERS: vk::ImageSubresourceLayers = vk::ImageSubresourceLayers {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    mip_level: 0,
    base_array_layer: 0,
    layer_count: 1,
};

fn rect2d(r: Rect) -> vk::Rect2D {
    vk::Rect2D {
        offset: vk::Offset2D { x: r.x, y: r.y },
        extent: vk::Extent2D { width: r.w.max(0) as u32, height: r.h.max(0) as u32 },
    }
}

impl Vulkan {
    /// Headless device (offscreen canvas). `attach` adds a window later.
    pub fn new(w: i32, h: i32) -> Result<Self, String> {
        // SAFETY: creation has no precondition beyond the module contract; it starts from the
        // loader alone.
        unsafe { Self::create(w, h) }
    }

    unsafe fn create(w: i32, h: i32) -> Result<Self, String> {
        let entry = entry()?;
        let available: Vec<String> = entry
            .enumerate_instance_extension_properties(None)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e.extension_name_as_c_str().ok().map(|c| c.to_string_lossy().into_owned()))
            .collect();
        let mut iexts = Vec::new();
        let want = |n: &std::ffi::CStr| available.iter().any(|a| a.as_bytes() == n.to_bytes());
        let has_surface_ext = want(ash::khr::surface::NAME);
        if has_surface_ext {
            iexts.push(ash::khr::surface::NAME.as_ptr());
            #[cfg(target_os = "linux")]
            if want(ash::khr::xlib_surface::NAME) {
                iexts.push(ash::khr::xlib_surface::NAME.as_ptr());
            }
            #[cfg(target_os = "linux")]
            if want(ash::khr::wayland_surface::NAME) {
                iexts.push(ash::khr::wayland_surface::NAME.as_ptr());
            }
            #[cfg(windows)]
            if want(ash::khr::win32_surface::NAME) {
                iexts.push(ash::khr::win32_surface::NAME.as_ptr());
            }
        }
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
        let inst = entry
            .create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&iexts),
                None,
            )
            .map_err(|e| format!("vkCreateInstance: {e}"))?;
        // A discrete GPU first, then integrated, then anything (llvmpipe).
        let pdevs = inst.enumerate_physical_devices().map_err(|e| format!("{e}"))?;
        let rank = |p: vk::PhysicalDevice| match inst.get_physical_device_properties(p).device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
            _ => 0,
        };
        let pdev = *pdevs.iter().max_by_key(|&&p| rank(p)).ok_or("no Vulkan device")?;
        let props = inst.get_physical_device_properties(pdev);
        let adapter = props.device_name_as_c_str().map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let qf = inst
            .get_physical_device_queue_family_properties(pdev)
            .iter()
            .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .ok_or("no graphics queue")? as u32;
        let prio = [1.0];
        let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(qf).queue_priorities(&prio)];
        let dexts = [ash::khr::swapchain::NAME.as_ptr()];
        let dexts: &[*const std::ffi::c_char] = if has_surface_ext { &dexts } else { &[] };
        let dev = inst
            .create_device(
                pdev,
                &vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_extension_names(dexts),
                None,
            )
            .map_err(|e| format!("vkCreateDevice: {e}"))?;
        let queue = dev.get_device_queue(qf, 0);
        let mem_props = inst.get_physical_device_memory_properties(pdev);
        let pool = dev
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
            .map_err(|e| format!("{e}"))?;
        let cmd = dev
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1),
            )
            .map_err(|e| format!("{e}"))?[0];
        let fence = dev.create_fence(&vk::FenceCreateInfo::default(), None).map_err(|e| format!("{e}"))?;

        // LOAD/STORE in GENERAL: a draw touches only its scissor, the rest of the target survives.
        let att = [vk::AttachmentDescription::default()
            .format(FMT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::GENERAL)
            .final_layout(vk::ImageLayout::GENERAL)];
        let cref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::GENERAL }];
        let sub = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&cref)];
        let rp = dev
            .create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&att).subpasses(&sub), None)
            .map_err(|e| format!("{e}"))?;

        // The WGSL bindings: g (uniform), atlas, tex_a, tex_b (textureLoad only: no samplers).
        let binds: Vec<vk::DescriptorSetLayoutBinding> = (0..4)
            .map(|i| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i)
                    .descriptor_type(if i == 0 {
                        vk::DescriptorType::UNIFORM_BUFFER
                    } else {
                        vk::DescriptorType::SAMPLED_IMAGE
                    })
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            })
            .collect();
        let dsl = dev
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binds), None)
            .map_err(|e| format!("{e}"))?;
        let pl = dev
            .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&[dsl]), None)
            .map_err(|e| format!("{e}"))?;
        let sizes = [
            vk::DescriptorPoolSize { ty: vk::DescriptorType::UNIFORM_BUFFER, descriptor_count: MAX_SETS },
            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 3 * MAX_SETS },
        ];
        let dpool = dev
            .create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default().max_sets(MAX_SETS).pool_sizes(&sizes),
                None,
            )
            .map_err(|e| format!("{e}"))?;
        // The built-in pipelines; image effects come with the first draw, one by one, so one that
        // does not compile here costs only its own draws (`draw`).
        let pipes = Self::pipelines(&dev, rp, pl, None)?;

        let null_buf =
            || Buf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), cap: 0 };
        let null_img = || Img {
            img: vk::Image::null(),
            mem: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            fb: vk::Framebuffer::null(),
            w: 0,
            h: 0,
        };
        let mut v = Vulkan {
            _entry: entry,
            inst,
            pdev,
            dev,
            mem_props,
            qf,
            queue,
            pool,
            cmd,
            fence,
            in_flight: false,
            recording: false,
            rp,
            dsl,
            pl,
            pipes,
            effects: 0,
            dpool,
            sets_used: 0,
            ubo: null_buf(),
            ubo_slot: 0,
            insts_buf: null_buf(),
            inst_used: 0,
            staging: null_buf(),
            staging_used: 0,
            dead_bufs: Vec::new(),
            dead: Vec::new(),
            atlas: null_img(),
            dummy: null_img(),
            canvas: null_img(),
            cw: w,
            ch: h,
            layers: HashMap::new(),
            images: HashMap::new(),
            maker: None,
            blur: Vec::new(),
            insts: Vec::new(),
            batches: Vec::new(),
            wsi: None,
            has_surface_ext,
            adapter,
            lost: false,
        };
        v.ubo = v.buffer(UBO_SLOT * UBO_SLOTS, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
        v.insts_buf = v.buffer(INST_SIZE * 16384, vk::BufferUsageFlags::VERTEX_BUFFER)?;
        v.staging = v.buffer(STAGING, vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)?;
        v.atlas = v.image(crate::text::ATLAS, crate::text::ATLAS, vk::Format::R8_UNORM, false)?;
        v.dummy = v.image(1, 1, FMT, false)?;
        v.canvas = v.image(bucket(w), bucket(h), FMT, true)?;
        Ok(v)
    }

    /// The pipelines of the built-in kinds, or (`effect`) of image effect `k` alone.
    unsafe fn pipelines(
        dev: &ash::Device,
        rp: vk::RenderPass,
        pl: vk::PipelineLayout,
        effect: Option<(usize, &'static crate::image::effect::EffectProgram)>,
    ) -> Result<Vec<vk::Pipeline>, String> {
        // The SPIR-V of an entry point, compiled when the program was built: the renderer's
        // (novaui's build) or the effect's (the app's, `effect_shaders!`).
        let code = |entry: &str| -> Result<&'static [u8], String> {
            match effect {
                None => gpu::spirv(entry).ok_or_else(|| format!("no SPIR-V for {entry}")),
                Some((_, p)) if entry == "vs" => Ok(p.gpu.vs),
                Some((_, p)) => Ok(p.gpu.fs),
            }
        };
        let spv = |entry: &str| -> Result<vk::ShaderModule, String> {
            let words = ash::util::read_spv(&mut std::io::Cursor::new(code(entry)?))
                .map_err(|e| format!("spv {entry}: {e}"))?;
            dev.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
                .map_err(|e| format!("{e}"))
        };
        let vs = spv("vs")?;
        let attrs: Vec<vk::VertexInputAttributeDescription> = (0..gpu::INST_ATTRS)
            .map(|i| vk::VertexInputAttributeDescription {
                location: i,
                binding: 0,
                format: vk::Format::R32G32B32A32_SFLOAT,
                offset: i * 16,
            })
            .collect();
        let vb = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: INST_SIZE as u32,
            input_rate: vk::VertexInputRate::INSTANCE,
        }];
        let vi = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&vb)
            .vertex_attribute_descriptions(&attrs);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let dyns = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyns);
        let mut out = Vec::new();
        let kinds: Vec<(u8, String)> = match effect {
            None => gpu::FS.iter().enumerate().map(|(k, e)| (k as u8, e.to_string())).collect(),
            Some((k, _)) => vec![(gpu::K_EFFECT + k as u8, gpu::EFFECT_ENTRY.to_string())],
        };
        for (k, e) in &kinds {
            let fs = spv(e)?;
            let fs_name = std::ffi::CString::new(e.as_str()).map_err(|_| format!("entry name {e:?}"))?;
            let stages = [
                vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs).name(c"vs"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(fs)
                    .name(&fs_name),
            ];
            // Clears and blur passes replace; everything else is premultiplied source-over.
            let blend = gpu::blends(*k);
            let cba = [vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(blend)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
            let ci = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vi)
                .input_assembly_state(&ia)
                .viewport_state(&vp)
                .rasterization_state(&rs)
                .multisample_state(&ms)
                .color_blend_state(&cb)
                .dynamic_state(&ds)
                .layout(pl)
                .render_pass(rp)
                .subpass(0);
            let p = dev
                .create_graphics_pipelines(vk::PipelineCache::null(), &[ci], None)
                .map_err(|e| format!("pipeline {e:?}", e = e.1))?[0];
            dev.destroy_shader_module(fs, None);
            out.push(p);
        }
        dev.destroy_shader_module(vs, None);
        Ok(out)
    }

    /// Window mode: a FIFO swap chain on the given surface.
    pub fn attach(&mut self, window: Window, w: i32, h: i32) -> Result<(), String> {
        if !self.has_surface_ext {
            return Err("VK_KHR_surface unavailable".into());
        }
        // SAFETY: the module contract; the window handles are the host's, alive while this
        // backend is attached to them (the host drops the backend before its window).
        unsafe {
            let surface = match window {
                #[cfg(target_os = "linux")]
                Window::Xlib { display, window } => {
                    let ci = vk::XlibSurfaceCreateInfoKHR::default().dpy(display as _).window(window as _);
                    ash::khr::xlib_surface::Instance::new(self._entry, &self.inst)
                        .create_xlib_surface(&ci, None)
                        .map_err(|e| format!("xlib surface: {e}"))?
                }
                #[cfg(target_os = "linux")]
                Window::Wayland { display, surface } => {
                    let ci = vk::WaylandSurfaceCreateInfoKHR::default().display(display as _).surface(surface as _);
                    ash::khr::wayland_surface::Instance::new(self._entry, &self.inst)
                        .create_wayland_surface(&ci, None)
                        .map_err(|e| format!("wayland surface: {e}"))?
                }
                #[cfg(windows)]
                Window::Win32 { hinstance, hwnd } => {
                    let ci = vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinstance).hwnd(hwnd);
                    ash::khr::win32_surface::Instance::new(self._entry, &self.inst)
                        .create_win32_surface(&ci, None)
                        .map_err(|e| format!("win32 surface: {e}"))?
                }
            };
            let surface_i = ash::khr::surface::Instance::new(self._entry, &self.inst);
            if !surface_i.get_physical_device_surface_support(self.pdev, self.qf, surface).unwrap_or(false) {
                surface_i.destroy_surface(surface, None);
                return Err("graphics queue cannot present to this surface".into());
            }
            let acquire = match self.dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                Ok(s) => s,
                Err(e) => {
                    surface_i.destroy_surface(surface, None);
                    return Err(format!("semaphore: {e}"));
                }
            };
            self.wsi = Some(Wsi {
                surface_i,
                surface,
                sc_d: ash::khr::swapchain::Device::new(&self.inst, &self.dev),
                sc: vk::SwapchainKHR::null(),
                images: Vec::new(),
                pending: Vec::new(),
                acquire,
                done: Vec::new(),
                format: vk::Format::B8G8R8A8_UNORM,
                extent: (0, 0),
                stale: true,
            });
            self.make_swapchain(w, h)
        }
    }

    unsafe fn make_swapchain(&mut self, w: i32, h: i32) -> Result<(), String> {
        self.retire();
        let _ = self.dev.device_wait_idle();
        let ws = self.wsi.as_mut().ok_or("no window attached")?;
        let caps = ws
            .surface_i
            .get_physical_device_surface_capabilities(self.pdev, ws.surface)
            .map_err(|e| format!("surface caps: {e}"))?;
        let fmts =
            ws.surface_i.get_physical_device_surface_formats(self.pdev, ws.surface).map_err(|e| format!("{e}"))?;
        let f = fmts
            .iter()
            .find(|f| f.format == FMT)
            .or_else(|| fmts.iter().find(|f| f.format == vk::Format::B8G8R8A8_UNORM))
            .copied()
            .ok_or("no 8-bit UNORM surface format")?;
        ws.format = f.format;
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D { width: w.max(1) as u32, height: h.max(1) as u32 }
        };
        if extent.width == 0 || extent.height == 0 {
            // Minimised: keep the old chain; present is skipped until the next size.
            ws.stale = true;
            return Ok(());
        }
        let count =
            (caps.min_image_count + 1).min(if caps.max_image_count == 0 { u32::MAX } else { caps.max_image_count });
        // Opaque where offered; NVIDIA's Wayland surface offers only INHERIT (the compositor
        // takes the buffer as the surface says, and ours is opaque).
        let alpha = [vk::CompositeAlphaFlagsKHR::OPAQUE, vk::CompositeAlphaFlagsKHR::INHERIT]
            .into_iter()
            .find(|a| caps.supported_composite_alpha.contains(*a))
            .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);
        let old = ws.sc;
        let ci = vk::SwapchainCreateInfoKHR::default()
            .surface(ws.surface)
            .min_image_count(count)
            .image_format(f.format)
            .image_color_space(f.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old);
        let sc = ws.sc_d.create_swapchain(&ci, None).map_err(|e| format!("swapchain: {e}"))?;
        if old != vk::SwapchainKHR::null() {
            ws.sc_d.destroy_swapchain(old, None);
        }
        ws.sc = sc;
        ws.images = ws.sc_d.get_swapchain_images(sc).map_err(|e| format!("{e}"))?;
        ws.pending = vec![None; ws.images.len()];
        for s in ws.done.drain(..) {
            self.dev.destroy_semaphore(s, None);
        }
        for _ in 0..ws.images.len() {
            match self.dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                Ok(s) => ws.done.push(s),
                Err(e) => {
                    // A chain short of its semaphores is not presented to: the next present
                    // remakes it.
                    ws.stale = true;
                    return Err(format!("semaphore: {e}"));
                }
            }
        }
        ws.extent = (extent.width as i32, extent.height as i32);
        ws.stale = false;
        Ok(())
    }

    unsafe fn buffer(&self, size: usize, usage: vk::BufferUsageFlags) -> Result<Buf, String> {
        new_buffer(&self.dev, &self.mem_props, size, usage)
    }

    unsafe fn free_buf(&self, b: &Buf) {
        free_buffer(&self.dev, b);
    }

    /// A device-local image in GENERAL layout (the transition is recorded, contents undefined).
    unsafe fn image(&mut self, w: i32, h: i32, fmt: vk::Format, rt: bool) -> Result<Img, String> {
        let Img { img, mem, view, w, h, .. } = new_image(&self.dev, &self.mem_props, w, h, fmt, rt)?;
        let fb = if rt {
            let atts = [view];
            self.dev
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(self.rp)
                        .attachments(&atts)
                        .width(w as u32)
                        .height(h as u32)
                        .layers(1),
                    None,
                )
                .map_err(|e| format!("{e}"))?
        } else {
            vk::Framebuffer::null()
        };
        self.record_general(img);
        Ok(Img { img, mem, view, fb, w, h })
    }

    /// Records a new image's move to GENERAL, where every image stays.
    unsafe fn record_general(&mut self, img: vk::Image) {
        self.begin();
        let b = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .image(img)
            .subresource_range(RANGE)
            .dst_access_mask(
                vk::AccessFlags::TRANSFER_WRITE
                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::SHADER_READ,
            );
        // A new image is first written by a copy (an upload) or a pass, or sampled.
        self.dev.cmd_pipeline_barrier(
            self.cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[b],
        );
    }

    unsafe fn destroy(&self, i: &Img) {
        destroy_image(&self.dev, i);
    }

    fn rt(&mut self, w: i32, h: i32) -> Option<Img> {
        // SAFETY: the module contract.
        match unsafe { self.image(bucket(w), bucket(h), FMT, true) } {
            Ok(i) => Some(i),
            Err(_) => {
                self.lost = true;
                None
            }
        }
    }

    /// Waits for the frame in flight (if any) and recycles everything it used.
    unsafe fn retire(&mut self) {
        if !self.in_flight {
            return;
        }
        if let Err(vk::Result::ERROR_DEVICE_LOST) = self.dev.wait_for_fences(&[self.fence], true, u64::MAX) {
            self.lost = true;
        }
        let _ = self.dev.reset_fences(&[self.fence]);
        self.in_flight = false;
        let _ = self.dev.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty());
        let _ = self.dev.reset_descriptor_pool(self.dpool, vk::DescriptorPoolResetFlags::empty());
        self.sets_used = 0;
        self.ubo_slot = 0;
        self.inst_used = 0;
        self.staging_used = 0;
        for d in std::mem::take(&mut self.dead) {
            self.destroy(&d);
        }
        for b in std::mem::take(&mut self.dead_bufs) {
            self.free_buf(&b);
        }
    }

    unsafe fn begin(&mut self) {
        if !self.recording {
            self.retire();
            let _ = self.dev.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            );
            self.recording = true;
        }
    }

    /// What was just written (`after`) is visible to what can use it next, and only that.
    unsafe fn barrier(&self, after: After) {
        use vk::AccessFlags as A;
        use vk::PipelineStageFlags as S;
        let (src, src_access, dst, dst_access) = match after {
            // A pass's colour writes: sampled (a layer, a blur level), blended over by the next
            // pass, copied out (to the swap chain, into a blur), or overwritten by a copy.
            After::Pass => (
                S::COLOR_ATTACHMENT_OUTPUT,
                A::COLOR_ATTACHMENT_WRITE,
                S::FRAGMENT_SHADER | S::COLOR_ATTACHMENT_OUTPUT | S::TRANSFER,
                A::SHADER_READ
                    | A::COLOR_ATTACHMENT_READ
                    | A::COLOR_ATTACHMENT_WRITE
                    | A::TRANSFER_READ
                    | A::TRANSFER_WRITE,
            ),
            // A copy into an image: sampled next or written by the next copy; the image it read
            // may be drawn into next (write after read).
            After::Copy => (
                S::TRANSFER,
                A::TRANSFER_WRITE,
                S::FRAGMENT_SHADER | S::COLOR_ATTACHMENT_OUTPUT | S::TRANSFER,
                A::SHADER_READ | A::COLOR_ATTACHMENT_WRITE | A::TRANSFER_WRITE,
            ),
            // A copy into a host-visible buffer, read by the CPU once the fence signals.
            After::Readback => (S::TRANSFER, A::TRANSFER_WRITE, S::HOST, A::HOST_READ),
        };
        let m = vk::MemoryBarrier::default().src_access_mask(src_access).dst_access_mask(dst_access);
        self.dev.cmd_pipeline_barrier(self.cmd, src, dst, vk::DependencyFlags::empty(), &[m], &[], &[]);
    }

    /// Submits what is recorded, optionally waiting/signalling presentation semaphores.
    unsafe fn submit(&mut self, wait: Option<vk::Semaphore>, signal: Option<vk::Semaphore>) {
        if !self.recording {
            return;
        }
        let _ = self.dev.end_command_buffer(self.cmd);
        self.recording = false;
        let cmds = [self.cmd];
        let waits: Vec<vk::Semaphore> = wait.into_iter().collect();
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let sigs: Vec<vk::Semaphore> = signal.into_iter().collect();
        let si = vk::SubmitInfo::default()
            .command_buffers(&cmds)
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages[..waits.len()])
            .signal_semaphores(&sigs);
        match self.dev.queue_submit(self.queue, &[si], self.fence) {
            Ok(()) => self.in_flight = true,
            Err(_) => self.lost = true,
        }
    }

    /// Submits and waits: the CPU may read results afterward.
    unsafe fn flush_wait(&mut self) {
        self.submit(None, None);
        self.retire();
    }

    /// Room for `n` more instances in this frame's ring; may submit the frame so far.
    unsafe fn reserve_insts(&mut self, n: usize) -> bool {
        if (self.inst_used + n) * INST_SIZE > self.insts_buf.cap {
            if self.inst_used > 0 {
                return false;
            }
            let Ok(b) = self.buffer((n * INST_SIZE).next_power_of_two(), vk::BufferUsageFlags::VERTEX_BUFFER) else {
                return false;
            };
            let old = std::mem::replace(&mut self.insts_buf, b);
            self.dead_bufs.push(old);
        }
        true
    }

    /// Callers made room first (`ensure_room`, which marks the device lost when it cannot).
    unsafe fn push_insts(&mut self, insts: &[Inst]) -> u32 {
        let first = self.inst_used;
        debug_assert!((first + insts.len()) * INST_SIZE <= self.insts_buf.cap, "instance ring overrun");
        std::ptr::copy_nonoverlapping(
            insts.as_ptr() as *const u8,
            self.insts_buf.ptr.add(first * INST_SIZE),
            insts.len() * INST_SIZE,
        );
        self.inst_used += insts.len();
        first as u32
    }

    unsafe fn push_globals(&mut self, g: [f32; 4]) -> u64 {
        let off = self.ubo_slot * UBO_SLOT;
        std::ptr::copy_nonoverlapping(g.as_ptr() as *const u8, self.ubo.ptr.add(off), 16);
        self.ubo_slot += 1;
        off as u64
    }

    /// The frame's rings can take one more draw call (else the frame so far is submitted).
    /// `false` when the instance ring could not grow (out of device memory): the device is then
    /// marked lost and the draw is dropped, since writing on would run past the mapped buffer.
    unsafe fn ensure_room(&mut self, insts: usize, sets: u32, ubos: usize) -> bool {
        if self.sets_used + sets > MAX_SETS
            || self.ubo_slot + ubos > UBO_SLOTS
            || (self.inst_used + insts) * INST_SIZE > self.insts_buf.cap
        {
            self.submit(None, None);
        }
        self.begin();
        if !self.reserve_insts(insts) {
            self.lost = true;
            return false;
        }
        true
    }

    /// A descriptor set for one draw; `None` (and the device lost) when the pool refuses.
    unsafe fn set(&mut self, ubo: u64, a: vk::ImageView, b: vk::ImageView) -> Option<vk::DescriptorSet> {
        let layouts = [self.dsl];
        let set = match self.dev.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.dpool).set_layouts(&layouts),
        ) {
            Ok(sets) => sets[0],
            Err(_) => {
                self.lost = true;
                return None;
            }
        };
        self.sets_used += 1;
        let ubi = [vk::DescriptorBufferInfo { buffer: self.ubo.buf, offset: ubo, range: 16 }];
        let views = [self.atlas.view, a, b];
        let infos: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&v| {
                [vk::DescriptorImageInfo {
                    sampler: vk::Sampler::null(),
                    image_view: v,
                    image_layout: vk::ImageLayout::GENERAL,
                }]
            })
            .collect();
        let mut w = vec![vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&ubi)];
        for (i, ii) in infos.iter().enumerate() {
            w.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32 + 1)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(ii),
            );
        }
        self.dev.update_descriptor_sets(&w, &[]);
        Some(set)
    }

    fn view(&self, t: TexRef) -> Option<vk::ImageView> {
        match t {
            TexRef::None => Some(self.dummy.view),
            TexRef::Layer(id) => self.layers.get(&id).map(|i| i.view),
            TexRef::Image(id) | TexRef::ImageHdr(id) => self.images.get(&id).map(|i| i.view),
        }
    }

    fn target(&self, t: Target) -> Option<(vk::Framebuffer, vk::Image, i32, i32)> {
        let i = match t {
            Target::Canvas => &self.canvas,
            Target::Layer(id) => self.layers.get(&id)?,
        };
        Some((i.fb, i.img, i.w, i.h))
    }

    unsafe fn begin_pass(&self, fb: vk::Framebuffer, tw: i32, th: i32, clip: Rect) {
        let rp = vk::RenderPassBeginInfo::default().render_pass(self.rp).framebuffer(fb).render_area(rect2d(clip));
        self.dev.cmd_begin_render_pass(self.cmd, &rp, vk::SubpassContents::INLINE);
        self.dev.cmd_set_viewport(
            self.cmd,
            0,
            &[vk::Viewport { x: 0.0, y: 0.0, width: tw as f32, height: th as f32, min_depth: 0.0, max_depth: 1.0 }],
        );
        self.dev.cmd_set_scissor(self.cmd, 0, &[rect2d(clip)]);
        self.dev.cmd_bind_vertex_buffers(self.cmd, 0, &[self.insts_buf.buf], &[0]);
    }

    unsafe fn end_pass(&self) {
        self.dev.cmd_end_render_pass(self.cmd);
        self.barrier(After::Pass);
    }

    /// Blurs the target under one Backdrop instance and draws the material over it. Called with
    /// no render pass open; leaves none open.
    unsafe fn backdrop(&mut self, target: Target, clip: Rect, i: &Inst) {
        let Some((fb, img, tw, th)) = self.target(target) else { return };
        let (rect, levels, extent) = gpu::backdrop_of(i);
        let region = backdrop_region(rect, extent, clip.intersect(&Rect::new(0, 0, tw, th)));
        if region.is_empty() {
            return;
        }
        let sizes = kawase_sizes(region.w, region.h, levels);
        for (k, &(w, h)) in sizes.iter().enumerate() {
            // Same reuse rule as the canvas: a chain grown for a large window is let go when the
            // window shrinks to under half of it.
            if !self.blur.get(k).is_some_and(|t| fits(t, w, h)) {
                let Some(t) = self.rt(w, h) else { return };
                if k < self.blur.len() {
                    let old = std::mem::replace(&mut self.blur[k], t);
                    self.dead.push(old);
                } else {
                    self.blur.push(t);
                }
            }
        }
        // Ring room was reserved by `draw` (BACKDROP_ROOM): submitting here would recycle the
        // instances the rest of this draw still refers to.
        let copy = vk::ImageCopy {
            src_subresource: LAYERS,
            src_offset: vk::Offset3D { x: region.x, y: region.y, z: 0 },
            dst_subresource: LAYERS,
            dst_offset: vk::Offset3D::default(),
            extent: vk::Extent3D { width: region.w as u32, height: region.h as u32, depth: 1 },
        };
        self.dev.cmd_copy_image(
            self.cmd,
            img,
            vk::ImageLayout::GENERAL,
            self.blur[0].img,
            vk::ImageLayout::GENERAL,
            &[copy],
        );
        self.barrier(After::Copy);
        let pass = |this: &mut Self, from: usize, to: usize, kind: u8| {
            let (w, h) = sizes[to];
            let first = this.push_insts(&[gpu::kawase_pass(&sizes, from, to)]);
            let ubo = this.push_globals([w as f32, h as f32, 0.0, 0.0]);
            let Some(set) = this.set(ubo, this.blur[from].view, this.dummy.view) else { return };
            this.begin_pass(this.blur[to].fb, w, h, Rect::new(0, 0, w, h));
            this.dev.cmd_bind_pipeline(this.cmd, vk::PipelineBindPoint::GRAPHICS, this.pipes[kind as usize]);
            this.dev.cmd_bind_descriptor_sets(this.cmd, vk::PipelineBindPoint::GRAPHICS, this.pl, 0, &[set], &[]);
            this.dev.cmd_draw(this.cmd, 4, 1, 0, first);
            this.end_pass();
        };
        for (from, to, up) in kawase_passes(levels) {
            pass(self, from, to, if up { gpu::K_UP } else { gpu::K_DOWN });
        }
        let first = self.push_insts(std::slice::from_ref(i));
        let ubo = self.push_globals([tw as f32, th as f32, region.x as f32, region.y as f32]);
        let Some(set) = self.set(ubo, self.blur[0].view, self.dummy.view) else { return };
        self.begin_pass(fb, tw, th, clip);
        self.dev.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.pipes[gpu::K_BACKDROP as usize]);
        self.dev.cmd_bind_descriptor_sets(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.pl, 0, &[set], &[]);
        self.dev.cmd_draw(self.cmd, 4, 1, 0, first);
        self.end_pass();
    }

    /// Acquire, copy exactly what this swap chain image is missing, present.
    unsafe fn present_window(&mut self, damage: &[Rect]) {
        let (cw, ch) = (self.cw, self.ch);
        let Some(ws) = self.wsi.as_mut() else { return };
        if ws.stale && (self.make_swapchain(cw, ch).is_err() || self.wsi.as_ref().is_none_or(|w| w.stale)) {
            self.submit(None, None);
            return;
        }
        let Some(ws) = self.wsi.as_mut() else { return };
        for p in ws.pending.iter_mut().flatten() {
            for r in damage {
                p.add(*r);
            }
        }
        let idx = match ws.sc_d.acquire_next_image(ws.sc, 1_000_000_000, ws.acquire, vk::Fence::null()) {
            Ok((i, _)) => i as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                ws.stale = true;
                self.submit(None, None);
                return;
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.lost = true;
                return;
            }
            Err(_) => {
                self.submit(None, None);
                return;
            }
        };
        let full = Rect::new(0, 0, cw.min(ws.extent.0), ch.min(ws.extent.1));
        let (rects, fresh): (Vec<Rect>, bool) = match ws.pending[idx].take() {
            Some(mut d) => {
                (d.take().into_iter().map(|r| r.intersect(&full)).filter(|r| !r.is_empty()).collect(), false)
            }
            None => (vec![full], true),
        };
        ws.pending[idx] = Some(Damage::default());
        let (img, acquire, done, format, sc) = (ws.images[idx], ws.acquire, ws.done[idx], ws.format, ws.sc);
        self.begin();
        let to_dst = vk::ImageMemoryBarrier::default()
            .old_layout(if fresh { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::PRESENT_SRC_KHR })
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(img)
            .subresource_range(RANGE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        self.dev.cmd_pipeline_barrier(
            self.cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_dst],
        );
        if !rects.is_empty() {
            if format == FMT {
                let copies: Vec<vk::ImageCopy> = rects
                    .iter()
                    .map(|r| vk::ImageCopy {
                        src_subresource: LAYERS,
                        src_offset: vk::Offset3D { x: r.x, y: r.y, z: 0 },
                        dst_subresource: LAYERS,
                        dst_offset: vk::Offset3D { x: r.x, y: r.y, z: 0 },
                        extent: vk::Extent3D { width: r.w as u32, height: r.h as u32, depth: 1 },
                    })
                    .collect();
                self.dev.cmd_copy_image(
                    self.cmd,
                    self.canvas.img,
                    vk::ImageLayout::GENERAL,
                    img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &copies,
                );
            } else {
                // BGRA swap chain: a 1:1 nearest blit swizzles the channels.
                let blits: Vec<vk::ImageBlit> = rects
                    .iter()
                    .map(|r| {
                        let o = [vk::Offset3D { x: r.x, y: r.y, z: 0 }, vk::Offset3D { x: r.r(), y: r.b(), z: 1 }];
                        vk::ImageBlit {
                            src_subresource: LAYERS,
                            src_offsets: o,
                            dst_subresource: LAYERS,
                            dst_offsets: o,
                        }
                    })
                    .collect();
                self.dev.cmd_blit_image(
                    self.cmd,
                    self.canvas.img,
                    vk::ImageLayout::GENERAL,
                    img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &blits,
                    vk::Filter::NEAREST,
                );
            }
        }
        let to_present = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .image(img)
            .subresource_range(RANGE)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        self.dev.cmd_pipeline_barrier(
            self.cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_present],
        );
        self.submit(Some(acquire), Some(done));
        let Some(ws) = self.wsi.as_mut() else { return };
        let scs = [sc];
        let idxs = [idx as u32];
        let sigs = [done];
        match ws.sc_d.queue_present(
            self.queue,
            &vk::PresentInfoKHR::default().wait_semaphores(&sigs).swapchains(&scs).image_indices(&idxs),
        ) {
            Ok(false) => {}
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => ws.stale = true,
            Err(vk::Result::ERROR_DEVICE_LOST) => self.lost = true,
            Err(_) => {}
        }
    }

    /// `n` bytes of upload space for this frame; `None` (and the device lost) when a buffer for
    /// it cannot be had.
    unsafe fn stage(&mut self, n: usize) -> Option<(vk::Buffer, *mut u8, u64)> {
        if self.staging_used + n <= self.staging.cap {
            let off = self.staging_used;
            self.staging_used = (off + n + 15) & !15;
            return Some((self.staging.buf, self.staging.ptr.add(off), off as u64));
        }
        // Larger than what is left: a one-frame buffer, freed when this frame retires.
        let Ok(b) = self.buffer(n, vk::BufferUsageFlags::TRANSFER_SRC) else {
            self.lost = true;
            return None;
        };
        let r = (b.buf, b.ptr, 0);
        self.dead_bufs.push(b);
        Some(r)
    }
}

impl Backend for Vulkan {
    fn name(&self) -> &'static str {
        "vk"
    }
    fn resize(&mut self, w: i32, h: i32) {
        if !fits(&self.canvas, w, h) {
            if let Some(c) = self.rt(w, h) {
                let old = std::mem::replace(&mut self.canvas, c);
                self.dead.push(old);
            }
        }
        self.cw = w;
        self.ch = h;
        if let Some(ws) = self.wsi.as_mut() {
            if surface_fit(ws.extent, (w, h)) != ws.extent {
                ws.stale = true;
            }
        }
    }
    fn atlas_size(&self) -> i32 {
        crate::text::ATLAS
    }
    fn upload_atlas(&mut self, r: Rect, atlas: &[u8], aw: i32) {
        // Each of `r`'s rows is read from `atlas` (`aw` pixels a row): they must lie inside it.
        if !atlas_rows_fit(r, aw, atlas.len()) {
            return;
        }
        // SAFETY: the module contract; `stage` gives `r.area()` writable bytes and each row copy
        // writes `r.w` of them at its own row, reading `r.w` bytes inside `atlas` (checked above).
        unsafe {
            self.begin();
            let Some((buf, p, off)) = self.stage(r.area() as usize) else { return };
            for y in 0..r.h {
                let s = ((r.y + y) * aw + r.x) as usize;
                std::ptr::copy_nonoverlapping(atlas[s..].as_ptr(), p.add((y * r.w) as usize), r.w as usize);
            }
            let reg = vk::BufferImageCopy {
                buffer_offset: off,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: LAYERS,
                image_offset: vk::Offset3D { x: r.x, y: r.y, z: 0 },
                image_extent: vk::Extent3D { width: r.w as u32, height: r.h as u32, depth: 1 },
            };
            self.dev.cmd_copy_buffer_to_image(self.cmd, buf, self.atlas.img, vk::ImageLayout::GENERAL, &[reg]);
            self.barrier(After::Copy);
        }
    }
    fn layer_alloc(&mut self, id: u32, w: i32, h: i32) {
        if self.layers.get(&id).is_some_and(|t| fits(t, w, h)) {
            return;
        }
        if let Some(t) = self.rt(w, h) {
            if let Some(old) = self.layers.insert(id, t) {
                self.dead.push(old);
            }
        }
    }
    fn layer_free(&mut self, id: u32) {
        if let Some(old) = self.layers.remove(&id) {
            self.dead.push(old);
        }
    }
    fn image_upload(&mut self, id: u32, w: i32, h: i32, rgba: &[u8]) {
        // SAFETY: the module contract; `stage(n)` gives `n` writable bytes and at most `n` are
        // copied from `rgba`, never more than it holds.
        unsafe {
            let Ok(img) = self.image(w, h, FMT, false) else {
                self.lost = true;
                return;
            };
            let n = (w * h * 4) as usize;
            let Some((buf, p, off)) = self.stage(n) else {
                self.dead.push(img);
                return;
            };
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), p, n.min(rgba.len()));
            let reg = vk::BufferImageCopy {
                buffer_offset: off,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: LAYERS,
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D { width: w.max(1) as u32, height: h.max(1) as u32, depth: 1 },
            };
            // A fresh image: its creation's layout barrier already orders this copy.
            self.dev.cmd_copy_buffer_to_image(self.cmd, buf, img.img, vk::ImageLayout::GENERAL, &[reg]);
            self.barrier(After::Copy);
            if let Some(old) = self.images.insert(id, img) {
                self.dead.push(old);
            }
        }
    }
    fn image_make(&mut self, img: &crate::image::ImageHandle, wake: Option<&Wake>) {
        let (id, w, h) = (img.id(), img.width(), img.height());
        if let Some(wake) = wake.filter(|_| make_apart(w, h)) {
            if self.maker.is_none() {
                self.maker = Maker::start(&self.dev, self.mem_props);
            }
            if let Some(m) = &mut self.maker {
                if m.jobs.as_ref().is_some_and(|j| j.send((img.clone(), wake.clone())).is_ok()) {
                    m.making.insert(id);
                    return;
                }
            }
        }
        self.image_upload(id, w, h, img.pixels());
    }
    fn images_made(&mut self, made: &mut Vec<u32>) {
        let Some(m) = &mut self.maker else { return };
        let mut came = Vec::new();
        while let Ok((id, t)) = m.done.try_recv() {
            if m.making.remove(&id) {
                came.push((id, t));
            } else if let Some(t) = t {
                // Freed before it came: never used by the GPU, gone at once.
                // SAFETY: the module contract; the maker made both with this device.
                unsafe {
                    destroy_image(&self.dev, &t.img);
                    free_buffer(&self.dev, &t.staging);
                }
            }
        }
        for (id, t) in came {
            let Some(Made { img, staging }) = t else { continue };
            // SAFETY: the module contract; the copy reads the staging buffer's `w * h * 4` bytes
            // (the maker filled them) into the image just moved to GENERAL; the buffer is freed
            // when this frame retires.
            unsafe {
                self.record_general(img.img);
                let reg = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: LAYERS,
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D { width: img.w as u32, height: img.h as u32, depth: 1 },
                };
                self.dev.cmd_copy_buffer_to_image(self.cmd, staging.buf, img.img, vk::ImageLayout::GENERAL, &[reg]);
                self.barrier(After::Copy);
            }
            self.dead_bufs.push(staging);
            if let Some(old) = self.images.insert(id, img) {
                self.dead.push(old);
            }
            made.push(id);
        }
    }
    fn image_free(&mut self, id: u32) {
        if let Some(m) = &mut self.maker {
            m.making.remove(&id);
        }
        if let Some(old) = self.images.remove(&id) {
            self.dead.push(old);
        }
    }
    fn draw(&mut self, target: Target, clip: Rect, off: (i32, i32), clear: Color, draws: &[Draw]) {
        for (k, p) in crate::image::effect::added_since(&mut self.effects) {
            // SAFETY: the module contract; the render pass and layout are this device's.
            match unsafe { Self::pipelines(&self.dev, self.rp, self.pl, Some((k, p))) } {
                Ok(p) => self.pipes.extend(p),
                Err(e) => {
                    // The app's program does not compile here: its draws are skipped (a null
                    // pipeline), the rest of the frame is drawn.
                    eprintln!("novaui[vk] image effect {k} pipeline: {e}");
                    self.pipes.push(vk::Pipeline::null());
                }
            }
        }
        let Some((fb, _, tw, th)) = self.target(target) else { return };
        let clip = clip.intersect(&Rect::new(0, 0, tw, th));
        if clip.is_empty() {
            return;
        }
        let clear = target.clear(clear);
        let mut insts = std::mem::take(&mut self.insts);
        let mut batches = std::mem::take(&mut self.batches);
        gpu::encode(clear, clip, off, draws, &mut insts, &mut batches);
        // SAFETY: the module contract; `ensure_room` makes the frame's rings hold every instance,
        // descriptor set and uniform this draw and its materials push (or the draw is dropped),
        // so each push writes inside its mapped buffer.
        unsafe {
            let backdrops = batches.iter().filter(|b| b.kind == gpu::K_BACKDROP).count();
            if !self.ensure_room(
                insts.len() + backdrops * BACKDROP_ROOM,
                (batches.len() + backdrops * BACKDROP_ROOM) as u32,
                1 + backdrops * BACKDROP_ROOM,
            ) {
                self.insts = insts;
                self.batches = batches;
                return;
            }
            let base = self.push_insts(&insts);
            let ubo = self.push_globals([tw as f32, th as f32, 0.0, 0.0]);
            let mut open = false;
            let mut bound: Option<(TexRef, TexRef)> = None;
            for b in &batches {
                if b.kind == gpu::K_BACKDROP {
                    if open {
                        self.end_pass();
                        open = false;
                    }
                    let i = insts[b.first as usize];
                    self.backdrop(target, clip, &i);
                    bound = None;
                    continue;
                }
                if !gpu::drawable(b.tex, |t| self.view(t).is_some()) {
                    continue;
                }
                let (Some(a), Some(bb)) = (self.view(b.tex.0), self.view(b.tex.1)) else { continue };
                let pipe = self.pipes[b.kind as usize];
                if pipe == vk::Pipeline::null() {
                    continue;
                }
                if !open {
                    self.begin_pass(fb, tw, th, clip);
                    open = true;
                    bound = None;
                }
                if bound != Some(b.tex) {
                    if self.sets_used >= MAX_SETS {
                        continue;
                    }
                    let Some(set) = self.set(ubo, a, bb) else { break };
                    self.dev.cmd_bind_descriptor_sets(
                        self.cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.pl,
                        0,
                        &[set],
                        &[],
                    );
                    bound = Some(b.tex);
                }
                self.dev.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
                self.dev.cmd_draw(self.cmd, 4, b.count, 0, base + b.first);
            }
            if open {
                self.end_pass();
            }
        }
        self.insts = insts;
        self.batches = batches;
    }
    fn present(&mut self, damage: &[Rect]) {
        // SAFETY: the module contract.
        unsafe {
            if self.wsi.is_some() {
                self.present_window(damage);
            } else {
                self.submit(None, None);
            }
        }
    }
    fn end_frame(&mut self) {
        // Resource-only turns (uploads, frees) still go to the GPU, so retired memory is freed.
        if self.recording {
            // SAFETY: the module contract; a command buffer is being recorded.
            unsafe { self.submit(None, None) };
        }
    }
    fn wait_frame(&mut self) {
        // SAFETY: the module contract.
        unsafe { self.retire() };
    }
    fn frame_ready(&mut self) -> bool {
        // SAFETY: the module contract; the fence is this device's.
        !self.in_flight || unsafe { self.dev.get_fence_status(self.fence).unwrap_or(true) }
    }
    fn finish(&mut self) {
        // SAFETY: the module contract.
        unsafe { self.flush_wait() };
    }
    fn adapter(&self) -> String {
        self.adapter.clone()
    }
    fn lost(&self) -> bool {
        self.lost
    }
    #[cfg(test)]
    fn resources(&self) -> Option<BackendResources> {
        let b = |i: &Img| i.w as u64 * i.h as u64 * 4;
        let image_bytes = self.images.values().map(b).sum::<u64>();
        let atlas_bytes = self.atlas.w as u64 * self.atlas.h as u64;
        let canvas_bytes = b(&self.canvas);
        let layer_bytes = self.layers.values().map(b).sum::<u64>();
        let scratch_bytes = self.blur.iter().map(b).sum::<u64>();
        Some(BackendResources {
            texture_bytes: atlas_bytes + canvas_bytes + layer_bytes + image_bytes + scratch_bytes,
            canvas_bytes,
            atlas_bytes,
            layer_bytes,
            image_bytes,
            scratch_bytes,
        })
    }
    fn readback(&mut self) -> Vec<u32> {
        let (w, h) = (self.cw, self.ch);
        let n = (w * h * 4) as usize;
        // SAFETY: the module contract; the buffer is host-visible, `n` bytes, mapped at `ptr`, and
        // read only after `flush_wait` has seen the copy into it complete.
        unsafe {
            let Ok(buf) = self.buffer(n, vk::BufferUsageFlags::TRANSFER_DST) else { return vec![0; (w * h) as usize] };
            self.begin();
            let reg = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: LAYERS,
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D { width: w as u32, height: h as u32, depth: 1 },
            };
            self.dev.cmd_copy_image_to_buffer(self.cmd, self.canvas.img, vk::ImageLayout::GENERAL, buf.buf, &[reg]);
            self.barrier(After::Readback);
            self.flush_wait();
            let px = std::slice::from_raw_parts(buf.ptr, n);
            let out = px.chunks_exact(4).map(argb_of_rgba8).collect();
            self.free_buf(&buf);
            out
        }
    }
}

/// What a barrier follows (`Vulkan::barrier`).
#[derive(Clone, Copy)]
enum After {
    Pass,
    Copy,
    Readback,
}

impl Drop for Vulkan {
    fn drop(&mut self) {
        // SAFETY: the module contract; recording is ended and the device idle before anything is
        // destroyed, children before the device, the device before the instance.
        unsafe {
            if self.recording {
                let _ = self.dev.end_command_buffer(self.cmd);
                self.recording = false;
            }
            self.retire();
            let _ = self.dev.device_wait_idle();
            // The maker's thread uses the device: it ends before anything goes, and what it made
            // and nobody took goes with the rest.
            if let Some(mut m) = self.maker.take() {
                m.jobs = None;
                if let Some(t) = m.thread.take() {
                    let _ = t.join();
                }
                for (_, made) in m.done.try_iter() {
                    if let Some(made) = made {
                        self.destroy(&made.img);
                        self.free_buf(&made.staging);
                    }
                }
            }
            for (_, l) in std::mem::take(&mut self.layers) {
                self.destroy(&l);
            }
            for (_, i) in std::mem::take(&mut self.images) {
                self.destroy(&i);
            }
            for b in std::mem::take(&mut self.blur) {
                self.destroy(&b);
            }
            for d in std::mem::take(&mut self.dead) {
                self.destroy(&d);
            }
            self.destroy(&self.canvas);
            self.destroy(&self.atlas);
            self.destroy(&self.dummy);
            for b in [&self.ubo, &self.insts_buf, &self.staging] {
                self.free_buf(b);
            }
            for b in std::mem::take(&mut self.dead_bufs) {
                self.free_buf(&b);
            }
            for &p in &self.pipes {
                self.dev.destroy_pipeline(p, None);
            }
            self.dev.destroy_pipeline_layout(self.pl, None);
            self.dev.destroy_descriptor_set_layout(self.dsl, None);
            self.dev.destroy_descriptor_pool(self.dpool, None);
            self.dev.destroy_render_pass(self.rp, None);
            self.dev.destroy_fence(self.fence, None);
            self.dev.destroy_command_pool(self.pool, None);
            if let Some(ws) = self.wsi.take() {
                ws.sc_d.destroy_swapchain(ws.sc, None);
                self.dev.destroy_semaphore(ws.acquire, None);
                for s in ws.done {
                    self.dev.destroy_semaphore(s, None);
                }
                self.dev.destroy_device(None);
                ws.surface_i.destroy_surface(ws.surface, None);
            } else {
                self.dev.destroy_device(None);
            }
            self.inst.destroy_instance(None);
        }
    }
}
