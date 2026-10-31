use super::*;

#[test]
fn native_release_commits_up_without_synthetic_focus_loss() {
    // A native window loads hook DLLs into the process (overlays, capture tools) that later fault
    // in other tests' Vulkan devices on the dev machine. The window gets a process of its own.
    if std::env::var_os("NOVAUI_CAPTURE_CHILD").is_none() {
        let me = std::env::current_exe().unwrap();
        let out = std::process::Command::new(me)
            .args(["platform::win32::capture_tests::native_release_commits_up_without_synthetic_focus_loss", "--exact"])
            .env("NOVAUI_CAPTURE_CHILD", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "child: {text}");
        return;
    }
    // Separate GUI thread; no cursor movement, window activation or input to other applications.
    // SAFETY: the windows are this thread's own, created and destroyed here; every structure
    // passed is a live local; `wndproc` runs on this thread as it does in the host.
    std::thread::spawn(|| unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = w!("NovaUI-Capture-Regression");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        });
        let a = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("capture test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            320,
            200,
            None,
            None,
            instance,
            None,
        )
        .unwrap();
        let b = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("capture other"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            320,
            200,
            None,
            None,
            instance,
            None,
        )
        .unwrap();
        QUEUE.with(|q| q.borrow_mut().clear());
        let pos = LPARAM((20 | (30 << 16)) as isize);
        SendMessageW(a, WM_LBUTTONDOWN, WPARAM(1), pos);
        assert_eq!(GetCapture(), a);
        SendMessageW(a, WM_LBUTTONUP, WPARAM(0), pos);
        let inputs = QUEUE
            .with(|q| std::mem::take(&mut *q.borrow_mut()))
            .into_iter()
            .filter_map(|m| match m {
                Msg::In(i) => Some(i),
                _ => None,
            })
            .collect::<Vec<_>>();
        // The press carries the modifier keys held as it went down (`Input::Mods`), then the
        // press and its release.
        assert_eq!(inputs.len(), 3, "unexpected native release inputs: {inputs:?}");
        assert!(matches!(inputs[0], Input::Mods(..)));
        assert!(matches!(inputs[1], Input::Down(..)));
        assert!(matches!(inputs[2], Input::Up(..)));
        SetCapture(a);
        QUEUE.with(|q| q.borrow_mut().clear());
        SetCapture(b);
        let inputs = QUEUE
            .with(|q| std::mem::take(&mut *q.borrow_mut()))
            .into_iter()
            .filter_map(|m| match m {
                Msg::In(i) => Some(i),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(inputs.iter().any(|i| matches!(i, Input::CaptureLost)));
        assert!(!inputs.iter().any(|i| matches!(i, Input::Focus(_))));
        let _ = ReleaseCapture();
        DestroyWindow(a).unwrap();
        DestroyWindow(b).unwrap();
        let _ = UnregisterClassW(class, instance);
    })
    .join()
    .unwrap();
}
