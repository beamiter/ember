//! A private real D-Bus portal, exercised through the public subscription API.
//! Run as a subprocess so changing the session address cannot affect other tests.
use crate::{async_rt, Interest, Preferences, ReducedMotion};
use futures_channel::mpsc as async_channel;
use futures_lite::StreamExt;
use std::{process::Command, sync::mpsc, thread, time::Duration};
use zbus::{message::Header, Connection, Proxy, Variant};

const DEADLINE: Duration = Duration::from_secs(10);
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "reduced-motion";
const TEST: &str = "callback::private_bus_test::subscription_drop_releases_private_bus_resources";
const CHILD: &str = "MUNDY_PRIVATE_BUS_TEST_CHILD";
const COMPLETED: &str = "MUNDY_PRIVATE_BUS_SUBSCRIPTION_LIFECYCLE_COMPLETED";

struct Settings(mpsc::Sender<String>);

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    fn read(
        &self,
        namespace: &str,
        key: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Variant<'static> {
        assert_eq!((namespace, key), (NAMESPACE, KEY));
        self.0
            .send(header.sender().expect("Read sender").to_string())
            .expect("report subscriber connection");
        // The real portal's legacy Read result is a variant inside a variant.
        Variant::Variant(Box::new(Variant::U32(0)))
    }
}

enum ServiceCommand {
    Change(u32),
    AwaitOwnerLoss(String),
    Stop,
}

enum ServiceEvent {
    Ready,
    ChangeProcessed,
    OwnerLost,
}

fn receive<T>(receiver: &mpsc::Receiver<T>, description: &str) -> T {
    receiver.recv_timeout(DEADLINE).expect(description)
}

async fn serve(
    mut commands: async_channel::UnboundedReceiver<ServiceCommand>,
    events: mpsc::Sender<ServiceEvent>,
    reads: mpsc::Sender<String>,
) {
    let connection = zbus::connection::Builder::session()
        .name("org.freedesktop.portal.Desktop")
        .serve_at(PATH, Settings(reads))
        .build()
        .await
        .expect("private portal service");
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    .expect("bus proxy");
    let mut owners = bus
        .receive_signal("NameOwnerChanged")
        .await
        .expect("owner watcher");
    events.send(ServiceEvent::Ready).expect("service ready");
    while let Some(command) = commands.next().await {
        match command {
            ServiceCommand::Change(value) => {
                emit(&connection, value).await;
                // A same-connection round trip is an explicit bus barrier after the signal.
                let _: Vec<String> = bus
                    .call("ListNames", &())
                    .await
                    .expect("signal bus barrier");
                events
                    .send(ServiceEvent::ChangeProcessed)
                    .expect("signal processed");
            }
            ServiceCommand::AwaitOwnerLoss(subscriber) => {
                loop {
                    let message = owners.next().await.expect("owner stream remains open");
                    let body = message.body();
                    let (name, old_owner, new_owner): (&str, &str, &str) =
                        body.deserialize().expect("NameOwnerChanged body");
                    if name == subscriber && old_owner == subscriber && new_owner.is_empty() {
                        break;
                    }
                }
                let exists: bool = bus
                    .call("NameHasOwner", &(subscriber,))
                    .await
                    .expect("owner absence barrier");
                assert!(!exists, "subscription connection must be released");
                events.send(ServiceEvent::OwnerLost).expect("owner lost");
            }
            ServiceCommand::Stop => break,
        }
    }
}

async fn emit(connection: &Connection, value: u32) {
    connection
        .emit_signal(
            None::<&str>,
            PATH,
            INTERFACE,
            "SettingChanged",
            &(NAMESPACE, KEY, Variant::U32(value)),
        )
        .await
        .expect("emit settings signal");
}

#[test]
fn subscription_drop_releases_private_bus_resources() {
    if std::env::var_os(CHILD).is_none() {
        // GNU timeout bounds even a deadlocked child/runtime shutdown. No sleeps or
        // timing-based success conditions are used inside the lifecycle assertions.
        let output = Command::new("timeout")
            .args(["--kill-after=5", "45", "dbus-run-session", "--"])
            .arg(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("run private D-Bus lifecycle test (requires dbus-run-session and timeout)");
        assert!(
            output.status.success(),
            "private D-Bus lifecycle child failed: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(COMPLETED),
            "child must execute the exact lifecycle test, not silently run zero tests"
        );
        return;
    }

    let (commands, command_stream) = async_channel::unbounded();
    let (events, event_stream) = mpsc::channel();
    let (reads, read_stream) = mpsc::channel();
    let service = thread::spawn(move || async_rt::block_on(serve(command_stream, events, reads)));
    assert!(matches!(
        receive(&event_stream, "portal ready"),
        ServiceEvent::Ready
    ));

    let finished = async_rt::subscription_test::observe();
    let (callbacks, callback_stream) = mpsc::channel();
    let subscription = Preferences::subscribe(Interest::ReducedMotion, move |preferences| {
        callbacks
            .send(preferences.reduced_motion)
            .expect("callback receiver");
    });
    let subscription_thread = async_rt::subscription_test::take_thread();
    let subscriber = receive(&read_stream, "portal Read from subscription");
    assert_eq!(
        receive(&callback_stream, "initial callback"),
        ReducedMotion::NoPreference
    );

    commands
        .unbounded_send(ServiceCommand::Change(1))
        .expect("change setting");
    assert!(matches!(
        receive(&event_stream, "first signal barrier"),
        ServiceEvent::ChangeProcessed
    ));
    assert_eq!(
        receive(&callback_stream, "changed callback"),
        ReducedMotion::Reduce
    );

    drop(subscription);
    receive(
        &finished,
        "subscription future and block_on runtime must finish",
    );
    // Joining after the bounded completion notification also waits for thread-local
    // runtime teardown. The outer timeout bounds thread-local destructor failures.
    subscription_thread
        .join()
        .expect("subscription thread exited cleanly");
    commands
        .unbounded_send(ServiceCommand::AwaitOwnerLoss(subscriber))
        .expect("await subscriber disconnect");
    assert!(matches!(
        receive(&event_stream, "D-Bus subscription connection released"),
        ServiceEvent::OwnerLost
    ));

    commands
        .unbounded_send(ServiceCommand::Change(0))
        .expect("post-drop signal");
    assert!(matches!(
        receive(&event_stream, "post-drop signal barrier"),
        ServiceEvent::ChangeProcessed
    ));
    assert!(
        matches!(
            callback_stream.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ),
        "callback must be dropped and no post-drop callback may be queued"
    );

    commands
        .unbounded_send(ServiceCommand::Stop)
        .expect("stop service");
    service
        .join()
        .expect("private portal thread exited cleanly");
    println!("{COMPLETED}");
}
