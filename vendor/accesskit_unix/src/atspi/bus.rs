// Ember zbus 6 compatibility patch; see ../../docs/linux-dbus-zbus6.md.
// Copyright 2022 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use crate::{
    atspi::{ObjectId, interfaces::*},
    context::get_or_init_app_context,
};
use accesskit_atspi_common::{
    NodeId, NodeIdOrRoot, ObjectEvent, PlatformNode, PlatformRoot, Property, WindowEvent,
};
use atspi::{
    Interface, InterfaceSet,
    events::EventBodyBorrowed,
    proxy::{bus::BusProxy, socket::SocketProxy},
};
use std::{env::var, io};
use zbus::{
    Address, Connection, Result, Str, Variant,
    connection::Builder,
    names::{BusName, InterfaceName, MemberName, OwnedUniqueName},
};

pub(crate) struct Bus {
    conn: Connection,
    socket_proxy: SocketProxy<'static>,
}

impl Bus {
    pub(crate) async fn new(session_bus: &Connection) -> zbus::Result<Self> {
        let address = match var("AT_SPI_BUS_ADDRESS") {
            Ok(address) if !address.is_empty() => address,
            _ => BusProxy::new(session_bus).await?.get_address().await?,
        };
        let address: Address = address.as_str().try_into()?;
        let conn = Builder::address(address).build().await?;
        let socket_proxy = SocketProxy::new(&conn).await?;
        let mut bus = Bus { conn, socket_proxy };
        bus.register_root_node().await?;
        Ok(bus)
    }

    pub(crate) async fn closed(&self) {
        self.conn.closed().await;
    }

    fn unique_name(&self) -> &OwnedUniqueName {
        self.conn.unique_name().unwrap()
    }

    async fn register_root_node(&mut self) -> Result<()> {
        let node = PlatformRoot::new(get_or_init_app_context());
        let path = ObjectId::Root.path();

        if self
            .conn
            .object_server()
            .at(path.clone(), ApplicationInterface(node.clone()))
            .await?
            .is_none()
        {
            self.socket_proxy
                .embed(&(self.unique_name().as_str(), ObjectId::Root.path().into()))
                .await?;

            self.conn
                .object_server()
                .at(
                    path,
                    RootAccessibleInterface::new(self.unique_name().to_owned(), node),
                )
                .await?;
        }

        Ok(())
    }

    pub(crate) async fn register_interfaces(
        &self,
        node: PlatformNode,
        new_interfaces: InterfaceSet,
    ) -> zbus::Result<()> {
        let path = ObjectId::from(&node).path();
        let bus_name = self.unique_name().to_owned();
        if new_interfaces.contains(Interface::Accessible) {
            self.register_interface(
                &path,
                NodeAccessibleInterface::new(bus_name.clone(), node.clone()),
            )
            .await?;
        }
        if new_interfaces.contains(Interface::Action) {
            self.register_interface(&path, ActionInterface::new(node.clone()))
                .await?;
        }
        if new_interfaces.contains(Interface::Component) {
            self.register_interface(
                &path,
                ComponentInterface::new(bus_name.clone(), node.clone()),
            )
            .await?;
        }
        if new_interfaces.contains(Interface::Hyperlink) {
            self.register_interface(
                &path,
                HyperlinkInterface::new(bus_name.clone(), node.clone()),
            )
            .await?;
        }
        if new_interfaces.contains(Interface::Selection) {
            self.register_interface(
                &path,
                SelectionInterface::new(bus_name.clone(), node.clone()),
            )
            .await?;
        }
        if new_interfaces.contains(Interface::Text) {
            self.register_interface(&path, TextInterface::new(node.clone()))
                .await?;
        }
        if new_interfaces.contains(Interface::Value) {
            self.register_interface(&path, ValueInterface::new(node.clone()))
                .await?;
        }

        Ok(())
    }

    async fn register_interface<T>(&self, path: &str, interface: T) -> Result<bool>
    where
        T: zbus::object_server::Interface,
    {
        map_or_ignoring_broken_pipe(
            self.conn.object_server().at(path, interface).await,
            false,
            |existing| existing.is_none(),
        )
    }

    pub(crate) async fn unregister_interfaces(
        &self,
        adapter_id: usize,
        node_id: NodeId,
        old_interfaces: InterfaceSet,
    ) -> zbus::Result<()> {
        let path = ObjectId::Node {
            adapter: adapter_id,
            node: node_id,
        }
        .path();
        if old_interfaces.contains(Interface::Accessible) {
            self.unregister_interface::<NodeAccessibleInterface>(&path)
                .await?;
        }
        if old_interfaces.contains(Interface::Action) {
            self.unregister_interface::<ActionInterface>(&path).await?;
        }
        if old_interfaces.contains(Interface::Component) {
            self.unregister_interface::<ComponentInterface>(&path)
                .await?;
        }
        if old_interfaces.contains(Interface::Hyperlink) {
            self.unregister_interface::<HyperlinkInterface>(&path)
                .await?;
        }
        if old_interfaces.contains(Interface::Selection) {
            self.unregister_interface::<SelectionInterface>(&path)
                .await?;
        }
        if old_interfaces.contains(Interface::Text) {
            self.unregister_interface::<TextInterface>(&path).await?;
        }
        if old_interfaces.contains(Interface::Value) {
            self.unregister_interface::<ValueInterface>(&path).await?;
        }

        Ok(())
    }

    async fn unregister_interface<T>(&self, path: &str) -> Result<bool>
    where
        T: zbus::object_server::Interface,
    {
        map_or_ignoring_broken_pipe(
            self.conn.object_server().remove::<T, _>(path).await,
            false,
            |result| result,
        )
    }

    pub(crate) async fn emit_object_event(
        &self,
        adapter_id: usize,
        target: NodeIdOrRoot,
        event: ObjectEvent,
    ) -> Result<()> {
        let target = match target {
            NodeIdOrRoot::Node(node) => ObjectId::Node {
                adapter: adapter_id,
                node,
            },
            NodeIdOrRoot::Root => ObjectId::Root,
        };
        let interface = "org.a11y.atspi.Event.Object";
        let signal = match event {
            ObjectEvent::ActiveDescendantChanged(_) => "ActiveDescendantChanged",
            ObjectEvent::Announcement(_, _) => "Announcement",
            ObjectEvent::BoundsChanged(_) => "BoundsChanged",
            ObjectEvent::CaretMoved(_) => "TextCaretMoved",
            ObjectEvent::ChildAdded(_, _) | ObjectEvent::ChildRemoved(_) => "ChildrenChanged",
            ObjectEvent::PropertyChanged(_) => "PropertyChange",
            ObjectEvent::SelectionChanged => "SelectionChanged",
            ObjectEvent::StateChanged(_, _) => "StateChanged",
            ObjectEvent::TextInserted { .. } | ObjectEvent::TextRemoved { .. } => "TextChanged",
            ObjectEvent::TextSelectionChanged => "TextSelectionChanged",
        };
        match event {
            ObjectEvent::ActiveDescendantChanged(child) => {
                let child = ObjectId::Node {
                    adapter: adapter_id,
                    node: child,
                };
                let mut body = EventBodyBorrowed::default();
                body.any_data = child.to_address(self.unique_name().inner()).into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::Announcement(message, politeness) => {
                let mut body = EventBodyBorrowed::default();
                body.detail1 = politeness as i32;
                body.any_data = message.into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::BoundsChanged(bounds) => {
                let mut body = EventBodyBorrowed::default();
                body.any_data = Variant::from(bounds);
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::CaretMoved(offset) => {
                let mut body = EventBodyBorrowed::default();
                body.detail1 = offset;
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::ChildAdded(index, child) => {
                let child = ObjectId::Node {
                    adapter: adapter_id,
                    node: child,
                };
                let mut body = EventBodyBorrowed::default();
                body.kind = "add";
                body.detail1 = index as i32;
                body.any_data = child.to_address(self.unique_name().inner()).into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::ChildRemoved(child) => {
                let child = ObjectId::Node {
                    adapter: adapter_id,
                    node: child,
                };
                let mut body = EventBodyBorrowed::default();
                body.kind = "remove";
                body.detail1 = -1;
                body.any_data = child.to_address(self.unique_name().inner()).into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::PropertyChanged(property) => {
                let mut body = EventBodyBorrowed::default();
                body.kind = match property {
                    Property::Name(_) => "accessible-name",
                    Property::Description(_) => "accessible-description",
                    Property::Parent(_) => "accessible-parent",
                    Property::Role(_) => "accessible-role",
                    Property::Value(_) => "accessible-value",
                };
                body.any_data = match property {
                    Property::Name(value) => Str::from(value).into(),
                    Property::Description(value) => Str::from(value).into(),
                    Property::Parent(parent) => {
                        let parent = match parent {
                            NodeIdOrRoot::Node(node) => ObjectId::Node {
                                adapter: adapter_id,
                                node,
                            },
                            NodeIdOrRoot::Root => ObjectId::Root,
                        };
                        parent.to_address(self.unique_name().inner()).into()
                    }
                    Property::Role(value) => Variant::U32(value as u32),
                    Property::Value(value) => Variant::F64(value),
                };
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::SelectionChanged => {
                self.emit_event(target, interface, signal, EventBodyBorrowed::default())
                    .await
            }
            ObjectEvent::StateChanged(state, value) => {
                let mut body = EventBodyBorrowed::default();
                body.kind = state.to_static_str();
                body.detail1 = value as i32;
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::TextInserted {
                start_index,
                length,
                content,
            } => {
                let mut body = EventBodyBorrowed::default();
                body.kind = "insert";
                body.detail1 = start_index;
                body.detail2 = length;
                body.any_data = content.into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::TextRemoved {
                start_index,
                length,
                content,
            } => {
                let mut body = EventBodyBorrowed::default();
                body.kind = "delete";
                body.detail1 = start_index;
                body.detail2 = length;
                body.any_data = content.into();
                self.emit_event(target, interface, signal, body).await
            }
            ObjectEvent::TextSelectionChanged => {
                self.emit_event(target, interface, signal, EventBodyBorrowed::default())
                    .await
            }
        }
    }

    pub(crate) async fn emit_window_event(
        &self,
        adapter_id: usize,
        target: NodeId,
        window_name: String,
        event: WindowEvent,
    ) -> Result<()> {
        let target = ObjectId::Node {
            adapter: adapter_id,
            node: target,
        };
        let signal = match event {
            WindowEvent::Activated => "Activate",
            WindowEvent::Deactivated => "Deactivate",
        };
        let mut body = EventBodyBorrowed::default();
        body.any_data = window_name.into();
        self.emit_event(target, "org.a11y.atspi.Event.Window", signal, body)
            .await
    }

    async fn emit_event(
        &self,
        target: ObjectId,
        interface: &str,
        signal_name: &str,
        body: EventBodyBorrowed<'_>,
    ) -> Result<()> {
        map_or_ignoring_broken_pipe(
            self.conn
                .emit_signal(
                    Option::<BusName>::None,
                    target.path(),
                    InterfaceName::from_str_unchecked(interface),
                    MemberName::from_str_unchecked(signal_name),
                    &body,
                )
                .await,
            (),
            |_| (),
        )
    }
}

pub(crate) fn map_or_ignoring_broken_pipe<T, U, F>(
    result: zbus::Result<T>,
    default: U,
    f: F,
) -> zbus::Result<U>
where
    F: FnOnce(T) -> U,
{
    match result {
        Ok(result) => Ok(f(result)),
        Err(zbus::Error::InputOutput(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
            Ok(default)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{atspi::OwnedObjectAddress, test_bus::PrivateBus, util::block_on};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use zbus::{ObjectPath, OwnedObjectPath, Type};

    struct RegistrySocket(Arc<AtomicUsize>);

    #[zbus::interface(name = "org.a11y.atspi.Socket")]
    impl RegistrySocket {
        fn embed(&self, plug: (String, OwnedObjectPath)) -> OwnedObjectAddress {
            assert!(plug.0.starts_with(':'));
            assert_eq!(plug.1.as_str(), ObjectId::Root.path().as_str());
            self.0.fetch_add(1, Ordering::SeqCst);
            OwnedObjectAddress::null()
        }
    }

    #[test]
    fn root_registers_once_and_exposes_protocol_properties() {
        let daemon = PrivateBus::new();
        block_on(async {
            let embeds = Arc::new(AtomicUsize::new(0));
            let _registry = Builder::address(daemon.address.as_str())
                .name("org.a11y.atspi.Registry")
                .serve_at(ObjectId::Root.path(), RegistrySocket(embeds.clone()))
                .build()
                .await
                .unwrap();
            let conn = Builder::address(daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let socket_proxy = SocketProxy::new(&conn).await.unwrap();
            let mut bus = Bus { conn, socket_proxy };
            bus.register_root_node().await.unwrap();
            bus.register_root_node().await.unwrap();
            assert_eq!(
                embeds.load(Ordering::SeqCst),
                1,
                "duplicate registration must not re-embed"
            );

            let client = Builder::address(daemon.address.as_str())
                .build()
                .await
                .unwrap();
            let proxy = zbus::Proxy::new(
                &client,
                bus.unique_name().as_str(),
                ObjectId::Root.path(),
                "org.a11y.atspi.Accessible",
            )
            .await
            .unwrap();
            let parent: (String, OwnedObjectPath) = proxy.get_property("Parent").await.unwrap();
            assert_eq!(parent.0, "");
            assert_eq!(parent.1.as_str(), "/org/a11y/atspi/null");
            let role: u32 = proxy.call("GetRole", &()).await.unwrap();
            assert_eq!(role, atspi::Role::Application as u32);
            let state: Vec<u32> = proxy.call("GetState", &()).await.unwrap();
            assert_eq!(state, vec![0, 0]);
            let interfaces: Vec<String> = proxy.call("GetInterfaces", &()).await.unwrap();
            assert_eq!(
                interfaces,
                vec!["org.a11y.atspi.Accessible", "org.a11y.atspi.Application"]
            );
            let invalid_child: zbus::Result<(OwnedObjectAddress,)> =
                proxy.call("GetChildAtIndex", &(-1_i32,)).await;
            match invalid_child.unwrap_err() {
                zbus::Error::FDO(error) => {
                    assert!(matches!(*error, zbus::fdo::Error::InvalidArgs(_)))
                }
                error => panic!("expected the AT-SPI InvalidArgs reply, got {error}"),
            }
            assert_eq!(OwnedObjectAddress::SIGNATURE.to_string(), "(so)");
            assert_eq!(<(String, ObjectPath<'_>)>::SIGNATURE.to_string(), "(so)");
        });
    }
}
