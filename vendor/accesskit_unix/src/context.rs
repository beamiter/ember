// Ember zbus 6 compatibility patch; see ../../docs/linux-dbus-zbus6.md.
// Copyright 2023 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use accesskit::{ActivationHandler, DeactivationHandler};
use accesskit_atspi_common::{Adapter as AdapterImpl, AppContext, Event};
#[cfg(not(feature = "tokio"))]
use async_channel::{Receiver, Sender};
use atspi::proxy::bus::StatusProxy;
#[cfg(not(feature = "tokio"))]
use futures_util::{StreamExt, pin_mut as pin, select};
use std::{
    sync::{Arc, Mutex, OnceLock, RwLock},
    thread,
};
#[cfg(feature = "tokio")]
use tokio::{
    pin, select,
    sync::mpsc::{UnboundedReceiver as Receiver, UnboundedSender as Sender},
};
#[cfg(feature = "tokio")]
use tokio_stream::{StreamExt, wrappers::UnboundedReceiverStream};
use zbus::{Connection, connection::Builder};

use crate::{
    adapter::{AdapterState, Callback, Message},
    atspi::{Bus, map_or_ignoring_broken_pipe},
    util::block_on,
};

static APP_CONTEXT: OnceLock<Arc<RwLock<AppContext>>> = OnceLock::new();
static MESSAGES: OnceLock<Sender<Message>> = OnceLock::new();

fn app_name() -> Option<String> {
    std::env::current_exe().ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
    })
}

pub(crate) fn get_or_init_app_context<'a>() -> &'a Arc<RwLock<AppContext>> {
    APP_CONTEXT.get_or_init(|| AppContext::new(app_name()))
}

pub(crate) fn get_or_init_messages() -> Sender<Message> {
    MESSAGES
        .get_or_init(|| {
            #[cfg(not(feature = "tokio"))]
            let (tx, rx) = async_channel::unbounded();
            #[cfg(feature = "tokio")]
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

            let callback_messages = tx.clone();
            thread::spawn(move || {
                // Drive both D-Bus connections for the entire lifetime of the
                // existing accessibility thread, rather than a helper runtime.
                block_on(async {
                    let result = match Builder::session().build().await {
                        Ok(session_bus) => run_event_loop(session_bus, rx, callback_messages).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        log::warn!("AccessKit accessibility connection stopped: {error}");
                    }
                })
            });

            tx
        })
        .clone()
}

struct AdapterEntry {
    id: usize,
    activation_handler: Box<dyn ActivationHandler>,
    deactivation_handler: Box<dyn DeactivationHandler>,
    state: Arc<Mutex<AdapterState>>,
}

fn activate_adapter(entry: &mut AdapterEntry, messages: &Sender<Message>) {
    let mut state = entry.state.lock().unwrap();
    if let AdapterState::Inactive {
        is_window_focused,
        root_window_bounds,
        action_handler,
    } = &*state
    {
        *state = match entry.activation_handler.request_initial_tree() {
            Some(initial_state) => {
                let r#impl = AdapterImpl::with_wrapped_action_handler(
                    entry.id,
                    get_or_init_app_context(),
                    Callback::with_messages(messages.clone()),
                    initial_state,
                    *is_window_focused,
                    *root_window_bounds,
                    Arc::clone(action_handler),
                );
                AdapterState::Active(r#impl)
            }
            None => AdapterState::Pending {
                is_window_focused: *is_window_focused,
                root_window_bounds: *root_window_bounds,
                action_handler: Arc::clone(action_handler),
            },
        };
    }
}

fn deactivate_adapter(entry: &mut AdapterEntry) {
    let mut state = entry.state.lock().unwrap();
    match &*state {
        AdapterState::Inactive { .. } => (),
        AdapterState::Pending {
            is_window_focused,
            root_window_bounds,
            action_handler,
        } => {
            *state = AdapterState::Inactive {
                is_window_focused: *is_window_focused,
                root_window_bounds: *root_window_bounds,
                action_handler: Arc::clone(action_handler),
            };
            drop(state);
            entry.deactivation_handler.deactivate_accessibility();
        }
        AdapterState::Active(r#impl) => {
            *state = AdapterState::Inactive {
                is_window_focused: r#impl.is_window_focused(),
                root_window_bounds: r#impl.root_window_bounds(),
                action_handler: r#impl.wrapped_action_handler(),
            };
            drop(state);
            entry.deactivation_handler.deactivate_accessibility();
        }
    }
}

async fn run_event_loop(
    session_bus: Connection,
    rx: Receiver<Message>,
    callback_messages: Sender<Message>,
) -> zbus::Result<()> {
    let status = StatusProxy::new(&session_bus).await?;
    let changes = status.receive_screen_reader_enabled_changed().await.fuse();
    pin!(changes);

    #[cfg(not(feature = "tokio"))]
    let messages = rx.fuse();
    #[cfg(feature = "tokio")]
    let messages = UnboundedReceiverStream::new(rx).fuse();
    pin!(messages);

    let mut atspi_bus: Option<Bus> = None;
    let mut adapters: Vec<AdapterEntry> = Vec::new();

    // Property streams may remain pending after a transport disconnect. Watch
    // both connections explicitly, and also handle a terminal stream without
    // repeatedly selecting None. All exits deactivate each adapter once.
    // Reconnection after a lost bus is deliberately outside this adapter port.
    let result = async {
        loop {
            enum Input<'a> {
                Status(Option<zbus::proxy::PropertyChanged<'a, bool>>),
                Message(Option<Message>),
                Disconnected,
            }
            // The losing future is dropped before handling the input, releasing
            // the accessibility-bus borrow before it can be replaced.
            let input = futures_lite::future::race(
                async {
                    select! {
                        change = changes.next() => Input::Status(change),
                        message = messages.next() => Input::Message(message),
                    }
                },
                async {
                    if let Some(bus) = &atspi_bus {
                        futures_lite::future::race(session_bus.closed(), bus.closed()).await;
                    } else {
                        session_bus.closed().await;
                    }
                    Input::Disconnected
                },
            )
            .await;
            match input {
                Input::Disconnected => {
                    return Err(zbus::Error::Failure("AT-SPI bus disconnected".into()));
                }
                Input::Status(change) => {
                    let Some(change) = change else {
                        return Err(zbus::Error::Failure("AT-SPI status stream closed".into()));
                    };
                    atspi_bus = None;
                    if change.get().await? {
                        atspi_bus =
                            map_or_ignoring_broken_pipe(Bus::new(&session_bus).await, None, Some)?;
                    }
                    for entry in &mut adapters {
                        if atspi_bus.is_some() {
                            activate_adapter(entry, &callback_messages);
                        } else {
                            deactivate_adapter(entry);
                        }
                    }
                }
                Input::Message(message) => {
                    let Some(message) = message else {
                        return Ok(());
                    };
                    process_adapter_message(&atspi_bus, &mut adapters, message, &callback_messages)
                        .await?;
                }
            }
        }
    }
    .await;
    for entry in &mut adapters {
        deactivate_adapter(entry);
    }
    result
}

async fn process_adapter_message(
    atspi_bus: &Option<Bus>,
    adapters: &mut Vec<AdapterEntry>,
    message: Message,
    callback_messages: &Sender<Message>,
) -> zbus::Result<()> {
    match message {
        #[cfg(test)]
        Message::Barrier(ack) => {
            let _ = ack.try_send(());
        }
        Message::AddAdapter {
            id,
            activation_handler,
            deactivation_handler,
            state,
        } => {
            adapters.push(AdapterEntry {
                id,
                activation_handler,
                deactivation_handler,
                state,
            });
            if atspi_bus.is_some() {
                let entry = adapters.last_mut().unwrap();
                activate_adapter(entry, callback_messages);
            }
        }
        Message::RemoveAdapter { id } => {
            if let Ok(index) = adapters.binary_search_by(|entry| entry.id.cmp(&id)) {
                adapters.remove(index);
            }
        }
        Message::RegisterInterfaces { node, interfaces } => {
            if let Some(bus) = atspi_bus {
                bus.register_interfaces(node, interfaces).await?
            }
        }
        Message::UnregisterInterfaces {
            adapter_id,
            node_id,
            interfaces,
        } => {
            if let Some(bus) = atspi_bus {
                bus.unregister_interfaces(adapter_id, node_id, interfaces)
                    .await?
            }
        }
        Message::EmitEvent {
            adapter_id,
            event: Event::Object { target, event },
        } => {
            if let Some(bus) = atspi_bus {
                bus.emit_object_event(adapter_id, target, event).await?
            }
        }
        Message::EmitEvent {
            adapter_id,
            event:
                Event::Window {
                    target,
                    name,
                    event,
                },
        } => {
            if let Some(bus) = atspi_bus {
                bus.emit_window_event(adapter_id, target, name, event)
                    .await?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{atspi::OwnedObjectAddress, test_bus::PrivateBus};
    use accesskit::{ActionHandler, ActionRequest, TreeUpdate};
    use accesskit_atspi_common::ActionHandlerWrapper;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zbus::OwnedObjectPath;

    struct Status(bool);
    #[zbus::interface(name = "org.a11y.Status")]
    impl Status {
        #[zbus(property)]
        fn is_enabled(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn screen_reader_enabled(&self) -> bool {
            self.0
        }
        #[zbus(property)]
        fn set_screen_reader_enabled(&mut self, enabled: bool) {
            self.0 = enabled;
        }
    }
    struct BusAddress(String);
    #[zbus::interface(name = "org.a11y.Bus")]
    impl BusAddress {
        fn get_address(&self) -> String {
            self.0.clone()
        }
    }
    struct Registry;
    #[zbus::interface(name = "org.a11y.atspi.Socket")]
    impl Registry {
        fn embed(&self, _plug: (String, OwnedObjectPath)) -> OwnedObjectAddress {
            OwnedObjectAddress::null()
        }
    }
    struct Activate(async_channel::Sender<&'static str>, Arc<AtomicUsize>);
    impl ActivationHandler for Activate {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.try_send("activated").unwrap();
            None
        }
    }
    struct Deactivate(async_channel::Sender<&'static str>, Arc<AtomicUsize>);
    impl DeactivationHandler for Deactivate {
        fn deactivate_accessibility(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.try_send("deactivated").unwrap();
        }
    }
    struct NoAction;
    impl ActionHandler for NoAction {
        fn do_action(&mut self, _request: ActionRequest) {
            panic!("lifecycle changes must not invoke application actions");
        }
    }

    fn check_shutdown(close_session: bool) {
        assert!(
            std::env::var("AT_SPI_BUS_ADDRESS")
                .unwrap_or_default()
                .is_empty(),
            "unset AT_SPI_BUS_ADDRESS: this fixture must only use its private bus"
        );
        let session_daemon = PrivateBus::new();
        let mut atspi_daemon = PrivateBus::new();
        block_on(async {
            let _session_service = Builder::address(session_daemon.address.as_str())
                .name("org.a11y.Bus")
                .serve_at("/org/a11y/bus", Status(true))
                .serve_at("/org/a11y/bus", BusAddress(atspi_daemon.address.clone()))
                .build()
                .await
                .unwrap();
            let _registry = Builder::address(atspi_daemon.address.as_str())
                .name("org.a11y.atspi.Registry")
                .serve_at("/org/a11y/atspi/accessible/root", Registry)
                .build()
                .await
                .unwrap();
            let conn = Builder::address(session_daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let control = Builder::address(session_daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let proxy = StatusProxy::new(&control).await.unwrap();
            #[cfg(not(feature = "tokio"))]
            let (tx, rx) = async_channel::unbounded();
            #[cfg(feature = "tokio")]
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let (notices, observations) = async_channel::unbounded();
            let activations = Arc::new(AtomicUsize::new(0));
            let deactivations = Arc::new(AtomicUsize::new(0));
            let state = Arc::new(Mutex::new(AdapterState::Inactive {
                is_window_focused: false,
                root_window_bounds: Default::default(),
                action_handler: Arc::new(ActionHandlerWrapper::new(NoAction)),
            }));
            let add = Message::AddAdapter {
                id: 1,
                activation_handler: Box::new(Activate(notices.clone(), activations.clone())),
                deactivation_handler: Box::new(Deactivate(notices, deactivations.clone())),
                state: state.clone(),
            };
            #[cfg(not(feature = "tokio"))]
            tx.try_send(add).unwrap();
            #[cfg(feature = "tokio")]
            tx.send(add).unwrap();
            let closing_conn = conn.clone();
            let (result, ()) =
                futures_lite::future::zip(run_event_loop(conn, rx, tx.clone()), async {
                    assert_eq!(observations.recv().await.unwrap(), "activated");
                    proxy.set_screen_reader_enabled(false).await.unwrap();
                    assert_eq!(observations.recv().await.unwrap(), "deactivated");
                    proxy.set_screen_reader_enabled(true).await.unwrap();
                    assert_eq!(observations.recv().await.unwrap(), "activated");
                    if close_session {
                        closing_conn.close().await.unwrap();
                    } else {
                        atspi_daemon.stop();
                    }
                    assert_eq!(observations.recv().await.unwrap(), "deactivated");
                })
                .await;
            assert!(result.is_err(), "transport loss must end the loop");
            assert!(matches!(
                *state.lock().unwrap(),
                AdapterState::Inactive { .. }
            ));
            assert_eq!(activations.load(Ordering::SeqCst), 2);
            assert_eq!(deactivations.load(Ordering::SeqCst), 2);
            assert!(
                observations.try_recv().is_err(),
                "no repeated callbacks after shutdown"
            );
            #[cfg(not(feature = "tokio"))]
            assert!(tx.try_send(Message::RemoveAdapter { id: 1 }).is_err());
            #[cfg(feature = "tokio")]
            assert!(tx.send(Message::RemoveAdapter { id: 1 }).is_err());
        });
    }

    #[test]
    fn session_loss_deactivates_once_and_ends_event_loop() {
        check_shutdown(true);
    }

    #[test]
    fn accessibility_bus_loss_deactivates_once_and_ends_event_loop() {
        check_shutdown(false);
    }

    // These tests share the production AppContext, so run active-tree cases
    // serially while each uses its own buses and callback queue.
    static ACTIVE_TREE_TEST: Mutex<()> = Mutex::new(());

    struct ActiveRegistry(async_channel::Sender<String>);
    #[zbus::interface(name = "org.a11y.atspi.Socket")]
    impl ActiveRegistry {
        fn embed(&self, plug: (String, OwnedObjectPath)) -> OwnedObjectAddress {
            self.0.try_send(plug.0).unwrap();
            OwnedObjectAddress::null()
        }
    }

    struct ActivateTree(async_channel::Sender<&'static str>);
    impl ActivationHandler for ActivateTree {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            use accesskit::{Action, Node, NodeId, Role, Tree, TreeId};
            let mut window = Node::new(Role::Window);
            window.set_label("Runtime lifetime fixture");
            window.set_children(vec![NodeId(2)]);
            let mut button = Node::new(Role::Button);
            button.set_label("Shutdown test button");
            button.add_action(Action::Click);
            self.0.try_send("activated").unwrap();
            Some(TreeUpdate {
                nodes: vec![(NodeId(1), window), (NodeId(2), button)],
                tree: Some(Tree::new(NodeId(1))),
                tree_id: TreeId::ROOT,
                focus: NodeId(2),
            })
        }
    }

    struct CountAction(Arc<AtomicUsize>);
    impl ActionHandler for CountAction {
        fn do_action(&mut self, request: ActionRequest) {
            assert_eq!(request.action, accesskit::Action::Click);
            assert_eq!(request.target_node, accesskit::NodeId(2));
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    async fn callback_barrier(tx: &Sender<Message>) {
        let (ack, received) = async_channel::bounded(1);
        #[cfg(not(feature = "tokio"))]
        tx.try_send(Message::Barrier(ack)).unwrap();
        #[cfg(feature = "tokio")]
        tx.send(Message::Barrier(ack)).unwrap();
        received.recv().await.unwrap();
    }

    fn check_active_tree_shutdown(close_session: bool) {
        let _serial = ACTIVE_TREE_TEST.lock().unwrap();
        assert!(
            std::env::var("AT_SPI_BUS_ADDRESS")
                .unwrap_or_default()
                .is_empty(),
            "unset AT_SPI_BUS_ADDRESS: this fixture must only use its private bus"
        );
        assert!(
            MESSAGES.get().is_none(),
            "the fixture must not start the global adapter thread"
        );
        let app_root = accesskit_atspi_common::PlatformRoot::new(get_or_init_app_context());
        assert_eq!(app_root.child_count().unwrap(), 0);
        let session_daemon = PrivateBus::new();
        let mut atspi_daemon = PrivateBus::new();
        block_on(async {
            let _session_service = Builder::address(session_daemon.address.as_str())
                .name("org.a11y.Bus")
                .serve_at("/org/a11y/bus", Status(true))
                .serve_at("/org/a11y/bus", BusAddress(atspi_daemon.address.clone()))
                .build()
                .await
                .unwrap();
            let (embedded, embeddings) = async_channel::unbounded();
            let _registry = Builder::address(atspi_daemon.address.as_str())
                .name("org.a11y.atspi.Registry")
                .serve_at("/org/a11y/atspi/accessible/root", ActiveRegistry(embedded))
                .build()
                .await
                .unwrap();
            let client = Builder::address(atspi_daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
            let mut owners = dbus.receive_name_owner_changed().await.unwrap();
            let conn = Builder::address(session_daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let closing_conn = conn.clone();
            #[cfg(not(feature = "tokio"))]
            let (tx, rx) = async_channel::unbounded();
            #[cfg(feature = "tokio")]
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let (notices, observations) = async_channel::unbounded();
            let deactivations = Arc::new(AtomicUsize::new(0));
            let actions = Arc::new(AtomicUsize::new(0));
            let state = Arc::new(Mutex::new(AdapterState::Inactive {
                is_window_focused: true,
                root_window_bounds: Default::default(),
                action_handler: Arc::new(ActionHandlerWrapper::new(CountAction(actions.clone()))),
            }));
            let id = accesskit_atspi_common::next_adapter_id();
            let add = Message::AddAdapter {
                id,
                activation_handler: Box::new(ActivateTree(notices.clone())),
                deactivation_handler: Box::new(Deactivate(notices, deactivations.clone())),
                state: state.clone(),
            };
            #[cfg(not(feature = "tokio"))]
            tx.try_send(add).unwrap();
            #[cfg(feature = "tokio")]
            tx.send(add).unwrap();
            let (result, retained_node) =
                futures_lite::future::zip(run_event_loop(conn, rx, tx.clone()), async {
                    assert_eq!(observations.recv().await.unwrap(), "activated");
                    let name = embeddings.recv().await.unwrap();
                    // Activation queues interface callbacks synchronously. The
                    // FIFO fence proves those real callbacks have all run.
                    callback_barrier(&tx).await;
                    assert!(matches!(*state.lock().unwrap(), AdapterState::Active(_)));
                    assert_eq!(app_root.child_count().unwrap(), 1);
                    let retained = app_root.child_at_index(0).unwrap().unwrap();
                    assert_eq!(retained.name().unwrap(), "Runtime lifetime fixture");
                    let root = zbus::Proxy::new(
                        &client,
                        name.as_str(),
                        "/org/a11y/atspi/accessible/root",
                        "org.a11y.atspi.Accessible",
                    )
                    .await
                    .unwrap();
                    let windows: Vec<(String, OwnedObjectPath)> =
                        root.call("GetChildren", &()).await.unwrap();
                    assert_eq!(windows.len(), 1);
                    let window = zbus::Proxy::new(
                        &client,
                        name.as_str(),
                        windows[0].1.as_str(),
                        "org.a11y.atspi.Accessible",
                    )
                    .await
                    .unwrap();
                    let children: Vec<(String, OwnedObjectPath)> =
                        window.call("GetChildren", &()).await.unwrap();
                    assert_eq!(children.len(), 1);
                    let button = zbus::Proxy::new(
                        &client,
                        name.as_str(),
                        children[0].1.as_str(),
                        "org.a11y.atspi.Accessible",
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        button.get_property::<String>("Name").await.unwrap(),
                        "Shutdown test button"
                    );
                    let action = zbus::Proxy::new(
                        &client,
                        name.as_str(),
                        children[0].1.as_str(),
                        "org.a11y.atspi.Action",
                    )
                    .await
                    .unwrap();
                    assert!(
                        action
                            .call::<_, _, bool>("DoAction", &(0_i32,))
                            .await
                            .unwrap()
                    );
                    assert_eq!(actions.load(Ordering::SeqCst), 1);
                    if close_session {
                        closing_conn.close().await.unwrap();
                    } else {
                        atspi_daemon.stop();
                    }
                    assert_eq!(observations.recv().await.unwrap(), "deactivated");
                    if close_session {
                        loop {
                            let signal = owners.next().await.expect("name-owner stream ended");
                            let args = signal.args().unwrap();
                            if args.name().as_str() == name && args.new_owner().is_none() {
                                break;
                            }
                        }
                        assert!(
                            !dbus
                                .name_has_owner(name.as_str().try_into().unwrap())
                                .await
                                .unwrap()
                        );
                    }
                    // This is an actual remote method, not a cached property.
                    assert!(
                        action
                            .call::<_, _, bool>("DoAction", &(0_i32,))
                            .await
                            .is_err()
                    );
                    retained
                })
                .await;
            assert!(
                result.is_err(),
                "transport loss must terminate the active-tree loop"
            );
            assert!(matches!(
                *state.lock().unwrap(),
                AdapterState::Inactive { .. }
            ));
            assert_eq!(deactivations.load(Ordering::SeqCst), 1);
            assert_eq!(
                actions.load(Ordering::SeqCst),
                1,
                "shutdown must not dispatch more actions"
            );
            assert_eq!(
                app_root.child_count().unwrap(),
                0,
                "AppContext must release the active tree"
            );
            assert!(
                matches!(
                    retained_node.name(),
                    Err(accesskit_atspi_common::Error::Defunct)
                ),
                "retained platform handles must no longer keep the tree alive"
            );
            assert!(
                observations.try_recv().is_err(),
                "no callbacks after shutdown"
            );
            #[cfg(not(feature = "tokio"))]
            assert!(tx.try_send(Message::RemoveAdapter { id }).is_err());
            #[cfg(feature = "tokio")]
            assert!(tx.send(Message::RemoveAdapter { id }).is_err());
        });
        assert!(MESSAGES.get().is_none());
    }

    #[test]
    fn active_tree_session_loss_releases_nodes_connection_and_callbacks() {
        check_active_tree_shutdown(true);
    }

    #[test]
    fn active_tree_accessibility_bus_loss_releases_nodes_and_callbacks() {
        check_active_tree_shutdown(false);
    }
}
