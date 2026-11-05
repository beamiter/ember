// Copyright 2026 The Ember contributors.
// Licensed under MIT OR Apache-2.0.

use std::{
    io::{BufRead, BufReader},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A real isolated D-Bus daemon. Tests never touch the desktop's session bus.
pub(crate) struct PrivateBus {
    child: Child,
    runtime_dir: PathBuf,
    pub(crate) address: String,
}

impl PrivateBus {
    pub(crate) fn stop(&mut self) {
        self.child.kill().expect("stop private bus");
        self.child.wait().expect("reap private bus");
    }

    pub(crate) fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let runtime_dir = std::env::temp_dir().join(format!(
            "ember-accesskit-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .expect("private bus runtime directory");
        let child = Command::new("dbus-daemon")
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .arg(format!("--address=unix:tmpdir={}", runtime_dir.display()))
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("protocol tests require dbus-daemon");
        // Construct the guard before any fallible read, so a failed launch
        // still reaps the child and removes the test's runtime directory.
        let mut bus = Self {
            child,
            runtime_dir,
            address: String::new(),
        };
        BufReader::new(bus.child.stdout.take().unwrap())
            .read_line(&mut bus.address)
            .unwrap();
        assert!(
            !bus.address.trim().is_empty(),
            "private bus address is missing"
        );
        bus.address = bus.address.trim().to_owned();
        bus
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}
