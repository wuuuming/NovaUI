//! Native minimize/maximize/restore regression (Win32 only). Operates only on its own HWND; no mouse input.
#[cfg(windows)]
mod win {
    use novaui::{
        dsl::{parse, Doc},
        platform as host, App, Ui,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::*,
    };

    #[derive(Clone, Debug)]
    struct Sample {
        phase: usize,
        size: (i32, i32),
        dip_width: f32,
        width: f32,
        tweening: bool,
    }
    struct Audit {
        doc: Doc,
        phase: Arc<AtomicUsize>,
        samples: Arc<Mutex<Vec<Sample>>>,
        /// Runs while the doc's tween does: each frame updates it again.
        tween: novaui::Ticker,
    }
    impl App for Audit {
        fn init(&mut self, ui: &mut Ui) {
            let root = ui.tree.root();
            self.doc.build(ui, root).unwrap();
            let (raw, phase) = (ui.window.handle(), self.phase.clone());
            // SAFETY: handles and plain values; ShowWindow, SendMessageW and PostMessageW take a
            // window of another thread and fail on one that is gone.
            std::thread::spawn(move || unsafe {
                let hwnd = HWND(raw as _);
                std::thread::sleep(std::time::Duration::from_millis(400));
                for (i, command) in
                    [SW_MINIMIZE, SW_RESTORE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_RESTORE].into_iter().enumerate()
                {
                    phase.store(i + 1, Ordering::Release);
                    let _ = ShowWindow(hwnd, command);
                    let _ = SendMessageW(hwnd, host::WM_APP_WAKE, WPARAM(0), LPARAM(0));
                    std::thread::sleep(std::time::Duration::from_millis(350));
                }
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            });
        }
        fn event(&mut self, _: &mut Ui, _: novaui::Event) {}
        fn nc_hit(&self, _: &Ui, _: i32, _: i32) -> novaui::app::NcHit {
            novaui::app::NcHit::Client
        }
        /// Every turn that delivered something (a resize, the tween's frame): a sample. A host
        /// that spins on work it cannot do shows here as a run of samples.
        fn after_layout(&mut self, ui: &mut Ui) {
            let tweening = self.doc.update(ui);
            self.tween.run(tweening);
            self.samples.lock().unwrap().push(Sample {
                phase: self.phase.load(Ordering::Acquire),
                size: (ui.tree.window_size().0, ui.tree.window_size().1),
                dip_width: ui.tree.window_size().0 as f32 / ui.scale(),
                width: self.doc.num("face_w"),
                tweening,
            });
        }
    }
    pub fn main() {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let data = samples.clone();
        host::run(
            host::Options {
                title: "NovaUI window-state regression".into(),
                width: 600,
                height: 420,
                min_width: 320,
                min_height: 200,
                backend: novaui::platform::BackendKind::D3d11,
                accessibility: false,
            },
            move |ui| {
                Box::new(Audit {
                    tween: ui.ticker(()),
                    doc: Doc::new(
                        parse(
                            r#"
                    latch wide { enter: window.w >= 800; keep: window.w >= 760; }
                    let face_w = tween(wide ? 400 : 200, wide, 300ms);
                    Box { width: face_w; height: 100; bg: #ff334455; }
                "#,
                        )
                        .unwrap(),
                    ),
                    phase: Arc::new(AtomicUsize::new(0)),
                    samples: data.clone(),
                })
            },
        )
        .expect("a window");
        let samples = samples.lock().unwrap();
        for phase in 1..=6 {
            let rows: Vec<_> = samples.iter().filter(|r| r.phase == phase).collect();
            assert!(!rows.is_empty(), "missing phase {phase}");
            assert!(rows.len() < 50, "phase {phase} spins: {} app turns", rows.len());
            let last = rows.last().unwrap();
            let want = if last.dip_width >= 800.0 { 400.0 } else { 200.0 };
            // Transitional native notifications may precede the SIZE message; the final state must
            // settle immediately, and no frame in a stable state may keep a law tween running.
            assert!(!last.tweening && (last.width - want).abs() < 0.01, "phase {phase}: {last:?}");
            if phase == 1 || phase == 4 {
                let prior = samples.iter().rev().find(|r| r.phase == phase - 1).unwrap();
                assert!(rows.iter().all(|r| r.size == prior.size), "minimized viewport changed");
            }
            println!(
                "phase {phase}: {} app turns, viewport {:?}, width {}, tweening {}",
                rows.len(),
                last.size,
                last.width,
                last.tweening
            );
        }
        println!("window states PASS");
    }
}

fn main() {
    #[cfg(windows)]
    win::main();
    #[cfg(not(windows))]
    eprintln!("window_states is a Win32 regression");
}
