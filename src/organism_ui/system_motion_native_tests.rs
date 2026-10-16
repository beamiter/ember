//! Hosted-only real portal protocol fixture; never reads a user's session bus.
use super::*;
use std::io::Write as _;
use std::sync::{atomic::AtomicUsize, Arc};

fn fixture_call(token: &str, method: &str, value: Option<bool>) -> String {
    let mut command = std::process::Command::new("gdbus");
    command.args([
        "call",
        "--timeout",
        "2",
        "--session",
        "--dest",
        "org.freedesktop.portal.Desktop",
        "--object-path",
        "/org/freedesktop/portal/desktop",
        "--method",
        &format!("io.github.beamiter.Ember.MotionFixture.{method}"),
        token,
    ]);
    if let Some(value) = value {
        command.arg(if value { "true" } else { "false" });
    }
    let output = command.output().expect("fixture control tool");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn await_condition(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "portal event deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(ctx: &egui::Context, repaints: &AtomicUsize) {
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
    }
    repaints.store(0, Ordering::SeqCst);
}

#[test]
#[ignore = "requires scripts/test-organism-system-motion.py owned hosted session bus"]
fn owned_portal_events_preserve_host_policy() {
    let token = std::env::var("EMBER_MOTION_FIXTURE_TOKEN").expect("owned fixture token");
    assert_eq!(std::env::var("EMBER_MOTION_OWNED_BUS").unwrap(), token);
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private session bus");
    assert!(!address.is_empty());
    assert_eq!(
        address,
        std::env::var("EMBER_MOTION_PRIVATE_BUS_ADDRESS").unwrap()
    );
    assert_ne!(
        address,
        std::env::var("EMBER_MOTION_PARENT_BUS_ADDRESS").unwrap()
    );
    let scenario = std::env::var("EMBER_MOTION_FIXTURE_SCENARIO").unwrap();
    assert!(matches!(
        scenario.as_str(),
        "appearance" | "gnome" | "missing"
    ));
    assert!(fixture_call(&token, "Reads", None).contains("uint32 0"));
    let ctx = egui::Context::default();
    let repaints = Arc::new(AtomicUsize::new(0));
    let counter = repaints.clone();
    ctx.set_request_repaint_callback(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    settle(&ctx, &repaints);
    let mut host = OrganismHost {
        owner: Some("fixture-local".into()),
        cursor: Some((42, false)),
        reaction_status: "completed successfully".into(),
        reaction_until: Duration::from_secs(12),
        ..Default::default()
    };
    let life_before = format!("{:?}", host.life.state());
    host.system_motion.connect(&ctx);
    let expected_reads = if scenario == "appearance" {
        "uint32 1"
    } else {
        "uint32 2"
    };
    await_condition(|| fixture_call(&token, "Reads", None).contains(expected_reads));
    if scenario == "missing" {
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(host.motion(None), OrganismMotion::Calm);
        assert_eq!(repaints.load(Ordering::SeqCst), 0);
    } else {
        await_condition(|| host.system_motion.reduced());
        await_condition(|| repaints.load(Ordering::SeqCst) == 1);
        assert_eq!(host.motion(None), OrganismMotion::Static);
        assert_eq!(repaints.load(Ordering::SeqCst), 1);
    }
    let policy = |motion| PresentationPolicy {
        enabled: true,
        focused_owner: true,
        local: true,
        motion: Some(motion),
        ..Default::default()
    };
    for reduced in [false, true, false, true] {
        settle(&ctx, &repaints);
        let changed = host.system_motion.reduced() != reduced;
        assert!(fixture_call(&token, "Emit", Some(reduced)).contains(&token));
        await_condition(|| host.system_motion.reduced() == reduced);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(repaints.load(Ordering::SeqCst), usize::from(changed));
        // Drain the first request so egui cannot hide a bad duplicate request.
        settle(&ctx, &repaints);
        assert!(fixture_call(&token, "Emit", Some(reduced)).contains(&token));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(repaints.load(Ordering::SeqCst), 0, "duplicate event");
        for explicit in [
            OrganismMotion::Full,
            OrganismMotion::Calm,
            OrganismMotion::Static,
        ] {
            assert_eq!(host.motion(Some(explicit)), explicit);
            assert_eq!(
                next_host_wake(
                    policy(host.motion(Some(explicit))),
                    Duration::from_secs(20),
                    host.reaction_until,
                    false
                ),
                match explicit {
                    OrganismMotion::Full => Some(Duration::from_millis(100)),
                    OrganismMotion::Calm => Some(Duration::from_millis(900)),
                    OrganismMotion::Static => None,
                }
            );
        }
        let p = policy(host.motion(None));
        assert_eq!(
            next_host_wake(p, Duration::from_secs(10), host.reaction_until, false),
            Some(if reduced {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(900)
            })
        );
        for seconds in [12, 13, 60, 3600] {
            assert_eq!(
                next_host_wake(p, Duration::from_secs(seconds), host.reaction_until, false),
                if reduced {
                    None
                } else {
                    Some(Duration::from_millis(900))
                }
            );
        }
        for index in 0..4 {
            let mut hidden = p;
            match index {
                0 => hidden.enabled = false,
                1 => hidden.focused_owner = false,
                2 => hidden.local = false,
                _ => hidden.alternate_screen = true,
            }
            assert_eq!(
                next_host_wake(hidden, Duration::from_secs(10), host.reaction_until, false),
                None
            );
        }
        assert_eq!(format!("{:?}", host.life.state()), life_before);
        assert_eq!(host.cursor, Some((42, false)));
        assert_eq!(host.owner.as_deref(), Some("fixture-local"));
        assert_eq!(host.reaction_status, "completed successfully");
    }
    // Queue a real signal immediately before drop; the callback may be in flight.
    let snapshot = host.system_motion.reduced.clone();
    fixture_call(&token, "Emit", Some(false));
    drop(host);
    await_condition(|| Arc::strong_count(&snapshot) == 1);
    settle(&ctx, &repaints);
    fixture_call(&token, "Emit", Some(true));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        repaints.load(Ordering::SeqCst),
        0,
        "callback after subscription teardown"
    );
    // A zero-test cargo filter must not be mistaken for acceptance.
    let marker = std::env::var("EMBER_MOTION_COMPLETION_MARKER").expect("fresh marker path");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .expect("fresh completion marker");
    file.write_all(token.as_bytes()).unwrap();
}
