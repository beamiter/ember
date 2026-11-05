// Ember zbus 6 compatibility patch; see ../../docs/linux-dbus-zbus6.md.
#[cfg(any(feature = "callback", target_os = "linux"))]
cfg_if::cfg_if! {
    if #[cfg(all(feature = "tokio", target_os = "linux"))] {
        #[allow(dead_code)]
        pub(crate) fn block_on<F>(future: F) -> F::Output
            where F: std::future::Future
        {
            // These are the features that zbus also activates:
            // <https://github.com/dbus2/zbus/blob/4e4151a9f9d745803d0337e1cd73e2b0f8eaec0d/zbus/src/utils.rs#L39>
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("launch of single-threaded tokio runtime");
            runtime.block_on(future)
        }
    } else if #[cfg(all(feature = "async-io", target_os = "linux"))] {
        #[allow(dead_code)]
        pub(crate) fn block_on<F>(future: F) -> F::Output
            where F: std::future::Future
        {
            // Drive zbus on this existing subscription thread for its entire lifetime.
            // A generic executor would make zbus start a separate fallback runtime.
            zbus::block_on(future)
        }
    } else if #[cfg(not(all(target_family = "wasm", target_os = "unknown")))] {
        #[allow(dead_code)]
        pub(crate) fn block_on<F>(future: F) -> F::Output
            where F: std::future::Future
        {
            beul::execute(future)
        }
    }
}

#[cfg(feature = "callback")]
cfg_if::cfg_if! {
    if #[cfg(all(target_family = "wasm", target_os = "unknown"))] {
        pub(crate) fn spawn_future(future: impl std::future::Future<Output = ()> + Send + 'static) {
            wasm_bindgen_futures::spawn_local(future);
        }
    } else {
        pub(crate) fn spawn_future(future: impl std::future::Future<Output = ()> + Send + 'static) {
            #[cfg(all(test, target_os = "linux", feature = "reduced-motion"))]
            let observer = subscription_test::take_observer();
            #[cfg(all(test, target_os = "linux", feature = "reduced-motion"))]
            let observed = observer.is_some();
            let thread = std::thread::Builder::new()
                .name(format!("{} subscription thread", env!("CARGO_PKG_NAME")))
                .spawn(move || {
                    block_on(future);
                    #[cfg(all(test, target_os = "linux", feature = "reduced-motion"))]
                    if let Some(observer) = observer {
                        let _ = observer.send(());
                    }
                })
                .expect("failed to spawn thread");
            #[cfg(all(test, target_os = "linux", feature = "reduced-motion"))]
            if observed {
                subscription_test::record_thread(thread);
            }
            #[cfg(not(all(test, target_os = "linux", feature = "reduced-motion")))]
            drop(thread);
        }
    }
}

// Per-caller hooks keep concurrent tests isolated. They observe the real callback thread;
// no executor, connection, or cancellation behavior is substituted in the test.
#[cfg(all(
    test,
    target_os = "linux",
    feature = "callback",
    feature = "reduced-motion"
))]
pub(crate) mod subscription_test {
    use std::{cell::RefCell, sync::mpsc, thread::JoinHandle};

    thread_local! {
        static OBSERVER: RefCell<Option<mpsc::Sender<()>>> = const { RefCell::new(None) };
        static THREAD: RefCell<Option<JoinHandle<()>>> = const { RefCell::new(None) };
    }

    pub fn observe() -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        OBSERVER.with(|slot| assert!(slot.replace(Some(sender)).is_none()));
        receiver
    }

    pub fn take_observer() -> Option<mpsc::Sender<()>> {
        OBSERVER.with(|slot| slot.take())
    }

    pub fn record_thread(thread: JoinHandle<()>) {
        THREAD.with(|slot| assert!(slot.replace(Some(thread)).is_none()));
    }

    pub fn take_thread() -> JoinHandle<()> {
        THREAD.with(|slot| slot.take().expect("subscription thread was spawned"))
    }
}
