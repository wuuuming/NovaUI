//! Equal pixels share one handle and one texture; replacing one element's image leaves the
//! others; the texture goes with its image's last handle.
use novaui::{render::primitive::Backend, render::software::Software, *};
/// Every GPU backend available on this machine (D3D11 on Windows, always: WARP at least; Vulkan
/// elsewhere, where a device is). A Linux machine without one (a CI runner) runs the software
/// half only.
#[cfg(windows)]
fn gpus(w: i32, h: i32) -> Vec<Box<dyn Backend>> {
    vec![Box::new(novaui::render::d3d11::D3d11::new(w, h).expect("D3D11 adapter"))]
}

#[cfg(not(windows))]
fn gpus(w: i32, h: i32) -> Vec<Box<dyn Backend>> {
    std::env::set_var("DISABLE_VULKAN_OBS_CAPTURE", "1");
    let mut v: Vec<Box<dyn Backend>> = Vec::new();
    if let Ok(g) = novaui::render::vulkan::Vulkan::new(w, h) {
        v.push(Box::new(g));
    }
    if v.is_empty() {
        eprintln!("no Vulkan device on this machine: the GPU backends' half of this test did not run");
    }
    v
}

fn image_node(ui: &mut Ui, image: &ImageHandle, x: f32) -> Id {
    ui.tree.add(
        ui.tree.root(),
        Layout {
            position: Position::Absolute,
            inset: [Len::Px(2.0), Len::Auto, Len::Auto, Len::Px(x)],
            width: Len::Px(18.0),
            height: Len::Px(18.0),
            ..Default::default()
        },
        Visual::default(),
        Content::Image(ImageContent { image: image.clone(), opacity: 1.0, fit_square: false, hdr: false }),
    )
}
fn check_pixels(ui: &mut Ui, be: &mut dyn Backend, now: f64) {
    let actual = be.readback();
    let mut reference = Software::new(ui.tree.window_size().0, ui.tree.window_size().1);
    ui.tree.invalidate_all();
    ui.tree.frame(&mut reference, now);
    let expected = reference.readback();
    let max = actual
        .iter()
        .zip(&expected)
        .flat_map(|(a, b)| [0, 8, 16, 24].map(|s| (((a >> s) & 255) as i32 - ((b >> s) & 255) as i32).unsigned_abs()))
        .max()
        .unwrap();
    assert!(max <= 2, "alias pixels differ from software by {max}");
    ui.tree.invalidate_all();
    ui.tree.frame(be, now);
}

#[test]
fn image_aliases_survive_replacement_release_and_backend_recreation() {
    let _device = super::device_test_lock();
    for (k, mut gpu) in gpus(96, 32).into_iter().enumerate() {
        let gpu = gpu.as_mut();
        let mut ui = Ui::new(96, 32, 1.0);
        let red_px: Vec<u8> = (0..16).flat_map(|i| [180 + i * 3, i * 7, 20, 255]).collect();
        let green_px: Vec<u8> = (0..16).flat_map(|i| [10, 160 + i * 4, i * 5, 255]).collect();
        let red = ImageHandle::rgba8(4, 4, red_px.clone());
        let a = image_node(&mut ui, &red, 2.0);
        let b = image_node(&mut ui, &ImageHandle::rgba8(4, 4, red_px.clone()), 24.0);
        ui.tree.frame(gpu, 0.0);
        assert_eq!(ui.tree.image_pixel_capacity(), 64);
        assert_eq!(ui.tree.last().images_up, 1);
        assert_eq!(gpu.resources().unwrap().image_bytes, 64);
        check_pixels(&mut ui, gpu, 0.0);

        // The first element changes; a new one still shows the original content.
        // Replacing through a shared texture would incorrectly turn the other rows green.
        let green = ImageHandle::rgba8(4, 4, green_px);
        ui.tree
            .set_content(a, Content::Image(ImageContent { image: green, opacity: 1.0, fit_square: false, hdr: false }));
        let c = image_node(&mut ui, &red, 46.0);
        ui.tree.frame(gpu, 1.0);
        assert_eq!(ui.tree.last().images_up, 1);
        assert_eq!(gpu.resources().unwrap().image_bytes, 128);
        check_pixels(&mut ui, gpu, 1.0);

        ui.tree.remove(b); // The node goes, while `c` keeps the red texture.
        ui.tree.frame(gpu, 2.0);
        assert_eq!(gpu.resources().unwrap().image_bytes, 128);
        check_pixels(&mut ui, gpu, 2.0);

        let d = image_node(&mut ui, &ImageHandle::rgba8(2, 8, red_px), 68.0);
        ui.tree.frame(gpu, 3.0);
        assert_eq!(gpu.resources().unwrap().image_bytes, 192, "different dimensions must not alias textures");
        check_pixels(&mut ui, gpu, 3.0);

        let mut recovered = gpus(96, 32).swap_remove(k);
        let recovered = recovered.as_mut();
        ui.tree.invalidate_all();
        ui.tree.frame(recovered, 4.0);
        assert_eq!(ui.tree.last().images_up, 3);
        assert_eq!(recovered.resources().unwrap().image_bytes, 192);
        check_pixels(&mut ui, recovered, 4.0);

        drop(red);
        for id in [a, c, d] {
            ui.tree.remove(id);
        }
        assert!(ui.tree.has_work());
        ui.tree.frame(recovered, 5.0);
        assert_eq!(ui.tree.image_pixel_capacity(), 0);
        assert_eq!(recovered.resources().unwrap().image_bytes, 0);
        check_pixels(&mut ui, recovered, 5.0);
        assert!(!ui.tree.has_work());
    }
}

/// An image no shown element draws (hidden, or gone while the app keeps its handle) gives its
/// texture back on the first turn `IMAGE_KEEP` later (no wake of its own), and is uploaded again
/// when an element shows it.
#[test]
fn textures_nothing_shows_are_freed_and_come_back() {
    let _device = super::device_test_lock();
    for mut gpu in gpus(96, 32) {
        let gpu = gpu.as_mut();
        let mut ui = Ui::new(96, 32, 1.0);
        let px: Vec<u8> = (0..16).flat_map(|i| [180 + i * 3, i * 7, 20, 255]).collect();
        let kept = ImageHandle::rgba8(4, 4, px);
        let a = image_node(&mut ui, &kept, 2.0);
        ui.tree.frame(gpu, 0.0);
        assert_eq!(gpu.resources().unwrap().image_bytes, 64);
        assert_eq!(ui.tree.image_free_at(), None, "a shown image has no deadline");

        // Hidden: the texture stays a while, then goes at its deadline (no frame asks for it).
        ui.tree.set_visible(a, false);
        ui.tree.frame(gpu, 1.0);
        assert_eq!(gpu.resources().unwrap().image_bytes, 64);
        let at = ui.tree.image_free_at().expect("a deadline for the hidden image");
        assert_eq!(at, 11.0);
        assert!(!ui.tree.has_work());
        ui.tree.serve_deadlines(at);
        assert!(ui.tree.has_work());
        ui.tree.frame(gpu, at);
        assert_eq!(gpu.resources().unwrap().image_bytes, 0);
        assert_eq!(ui.tree.image_free_at(), None);
        assert!(!ui.tree.has_work());

        // Shown again: uploaded again, drawn as before.
        ui.tree.set_visible(a, true);
        ui.tree.frame(gpu, 12.0);
        assert_eq!(ui.tree.last().images_up, 1);
        assert_eq!(gpu.resources().unwrap().image_bytes, 64);
        check_pixels(&mut ui, gpu, 12.0);

        // Gone while the app keeps the handle: the same.
        ui.tree.remove(a);
        ui.tree.frame(gpu, 13.0);
        assert_eq!(gpu.resources().unwrap().image_bytes, 64);
        let at = ui.tree.image_free_at().expect("a deadline for the image nothing draws");
        ui.tree.serve_deadlines(at);
        ui.tree.frame(gpu, at);
        assert_eq!(gpu.resources().unwrap().image_bytes, 0);
        drop(kept);
    }
}
