//! Actual bridge exercise for the frontend's shared paint/input controller.
use super::*;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

struct Fixture {
    session: Vim9Session,
    master: fs::File,
    child: std::process::Child,
    root: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the real Vim bridge"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Fixture {
    fn start() -> Self {
        let root =
            std::env::temp_dir().join(format!("ember-native-input-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("fixture.txt");
        let text = format!(
            "first row\nsecond ordinary row\nA中e\u{301}Z\n{}",
            (4..=120)
                .map(|line| format!("row {line} enough source for scrolling\n"))
                .collect::<String>()
        );
        fs::write(&file, text).unwrap();
        let vimrc = root.join("fixture.vim");
        fs::write(&vimrc, "set nocompatible nomore noswapfile nowrap mouse=a nonumber norelativenumber laststatus=0\nset nospell nolist conceallevel=0\nset runtimepath=$VIMRUNTIME packpath= noloadplugins\n").unwrap();
        let session =
            Vim9Session::start(jterm_core::vim9::Vim9EntryRequest::user_action(file)).unwrap();
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: 20,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty writes owned file descriptors into valid pointers.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        // SAFETY: each descriptor returned by openpty is consumed exactly once.
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let launch = session.launch();
        let mut command = std::process::Command::new(&launch.program);
        command
            .args(["-u", vimrc.to_str().unwrap(), "-i", "NONE", "-n"])
            .args(&launch.args)
            .current_dir(&root)
            .env("TERM", "xterm-256color")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        for entry in &launch.env {
            let (name, value) = entry.split_once('=').unwrap();
            command.env(name, value);
        }
        // SAFETY: only async-signal-safe libc calls run before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut drain = master.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while matches!(drain.read(&mut bytes), Ok(1..)) {}
        });
        let fixture = Self {
            session,
            master,
            child,
            root,
        };
        wait(|| fixture.session.snapshot().connected);
        fixture
            .session
            .send(Vim9Command::SetRender { enabled: true })
            .unwrap();
        wait(|| {
            fixture.session.render_snapshot().is_some_and(|frame| {
                frame.supported
                    && frame.mode == "n"
                    && frame.windows.iter().any(|window| window.native_pointer)
            }) && fixture.session.snapshot().pending == 0
        });
        fixture
    }
}

#[test]
#[ignore = "requires real Vim 9, a PTY, and local bridge sockets"]
fn real_vim_fractional_click_drag_yank_and_cancel_use_the_presented_source() {
    let mut fixture = Fixture::start();
    let geometry = Vim9RenderMetrics {
        origin_x: 0.0,
        origin_y: 0.0,
        cell_width: 8.0,
        line_height: 20.0,
    };
    let frame = fixture.session.render_snapshot().unwrap();
    let window = frame.window;
    let mut input = DirectInput::default();
    let ctx = egui::Context::default();
    assert!(input.wheel(&frame, window, geometry, 0.3125, &fixture.session));
    assert_eq!(input.offset(&frame, window, geometry), -6.25);
    assert_eq!(
        fixture.session.snapshot().pending,
        0,
        "a fractional pixel gesture must paint before any whole-line request"
    );
    input.remember_presentation(Arc::clone(&frame), geometry, &ctx);
    let click = egui::pos2(25.0, 17.0);
    let target = input.presented_anchor(&frame, geometry, click).unwrap();
    assert_eq!((target.position.line, target.position.byte_column), (2, 3));
    assert!(input.press(&frame, geometry, click, 1.0, &fixture.session));
    let drag = egui::pos2(25.0, 37.0);
    let target = input.presented_anchor(&frame, geometry, drag).unwrap();
    assert_eq!(
        (target.position.line, target.position.byte_column),
        (3, 4),
        "wide/combining text maps to UTF-8 bytes"
    );
    for _ in 0..100 {
        input.pointer(&frame, geometry, Some(drag), false, &fixture.session);
    }
    assert!(
        fixture.session.snapshot().pending <= 1,
        "pointer motion must coalesce behind its single in-flight request"
    );
    input.pointer(&frame, geometry, Some(drag), true, &fixture.session);
    wait(|| {
        let snapshot = fixture.session.input_snapshot();
        let next = snapshot.frame.as_ref().unwrap();
        input.reconcile(&snapshot, geometry);
        input.pointer(next, geometry, None, true, &fixture.session);
        !input.pointer_captured() && fixture.session.snapshot().pending == 0
    });
    let selected = fixture.session.render_snapshot().unwrap();
    assert_eq!(selected.mode, "v");
    assert_eq!((selected.cursor.line, selected.cursor.byte_column), (3, 4));
    assert_eq!(
        input.offset(&selected, window, geometry),
        -6.25,
        "a source-pointer selection must retain the fractional image throughout Visual mode"
    );
    let yank_file = fixture.root.join("yank.json");
    fixture
        .master
        .write_all(
            format!(
                "y:call writefile([json_encode(getreg('\"'))], '{}')\r",
                yank_file.display()
            )
            .as_bytes(),
        )
        .unwrap();
    // writefile() creates the path before its bytes are visible to this
    // process. Wait for a complete JSON value rather than the creation alone.
    wait(|| {
        fs::read_to_string(&yank_file)
            .is_ok_and(|text| serde_json::from_str::<String>(&text).is_ok())
    });
    let yank: String = serde_json::from_str(&fs::read_to_string(yank_file).unwrap()).unwrap();
    assert_eq!(yank, "ond ordinary row\nA中e\u{301}");

    wait(|| {
        fixture
            .session
            .render_snapshot()
            .is_some_and(|frame| frame.mode == "n")
    });
    let next = fixture.session.render_snapshot().unwrap();
    input.cancel();
    assert!(input.wheel(&next, next.window, geometry, 0.25, &fixture.session));
    input.remember_presentation(Arc::clone(&next), geometry, &ctx);
    assert!(input.press(
        &next,
        geometry,
        egui::pos2(9.0, 17.0),
        3.0,
        &fixture.session
    ));
    wait(|| fixture.session.snapshot().pending == 0);
    input.cancel();
    fixture.master.write_all(b"\x1b").unwrap();
    assert!(input.pointer(&next, geometry, Some(drag), true, &fixture.session));
    assert!(!input.pointer_captured());
    assert_eq!(
        fixture.session.snapshot().pending,
        0,
        "a cancelled drag cannot submit a delayed release target"
    );
}

#[test]
#[ignore = "requires real Vim 9, a PTY, and local bridge sockets"]
fn real_vim_precision_scroll_with_virtualedit_and_insert_mode_ignores_notch_animation_preference() {
    let mut fixture = Fixture::start();
    fixture
        .master
        .write_all(b":set virtualedit=block | nnoremap <LeftMouse> <LeftMouse>\r")
        .unwrap();
    wait(|| {
        fixture.session.render_snapshot().is_some_and(|frame| {
            frame.supported
                && frame.mode == "n"
                && frame
                    .windows
                    .iter()
                    .all(|window| window.native_scroll && !window.native_pointer)
        }) && fixture.session.snapshot().pending == 0
    });
    let geometry = Vim9RenderMetrics {
        origin_x: 0.0,
        origin_y: 0.0,
        cell_width: 8.0,
        line_height: 20.0,
    };
    for mode in ["n", "i"] {
        if mode == "i" {
            fixture.master.write_all(b"i").unwrap();
            wait(|| {
                fixture
                    .session
                    .render_snapshot()
                    .is_some_and(|frame| frame.supported && frame.mode == "i")
            });
        }
        let frame = fixture.session.render_snapshot().unwrap();
        let window = frame.window;
        assert!(frame.windows.iter().all(|window| window.native_scroll));
        assert!(frame.windows.iter().all(|window| !window.native_pointer));
        let mut input = DirectInput::default();
        input.set_smooth(false);
        assert!(input.wheel(&frame, window, geometry, 0.3125, &fixture.session));
        assert_eq!(input.offset(&frame, window, geometry), -6.25);
        assert_eq!(fixture.session.snapshot().pending, 0);
        input.remember_presentation(Arc::clone(&frame), geometry, &egui::Context::default());
        assert_eq!(
            input.wheel_window_at(&frame, geometry, egui::pos2(25.0, 17.0)),
            Some(window)
        );
        assert!(input
            .presented_anchor(&frame, geometry, egui::pos2(25.0, 17.0))
            .is_none());
        assert!(input.wheel(&frame, window, geometry, 1.0, &fixture.session));
        wait(|| {
            let snapshot = fixture.session.input_snapshot();
            input.reconcile(&snapshot, geometry);
            fixture.session.snapshot().pending == 0 && !input.needs_frame()
        });
        let acknowledged = fixture.session.render_snapshot().unwrap();
        assert_eq!(acknowledged.mode, mode);
        assert_eq!(
            acknowledged.windows[0].topline,
            frame.windows[0].topline + 1
        );
        assert_eq!(input.offset(&acknowledged, window, geometry), -6.25);
    }
}
