// Ember zbus 6 compatibility patch; see ../../docs/linux-dbus-zbus6.md.
use crate::{async_rt, Interest, Preferences};
use futures_channel::oneshot;
use futures_lite::{stream, StreamExt as _};

pub trait CallbackFn: FnMut(Preferences) + Send + Sync + 'static {}

impl<F> CallbackFn for F where F: FnMut(Preferences) + Send + Sync + 'static {}

/// A subscription for preferences created using [`Preferences::subscribe()`].
/// Dropping the subscription will cancel it and clean up all associated resources.
pub struct Subscription(
    #[cfg_attr(
        not(all(test, target_os = "linux")),
        expect(dead_code, reason = "only used to send a canceled message on drop")
    )]
    Option<oneshot::Sender<()>>,
);

#[cfg(test)]
static_assertions::assert_impl_all!(Subscription: Send, Sync);

impl Preferences {
    pub(crate) fn subscribe_impl(
        interest: Interest,
        mut callback: impl CallbackFn,
    ) -> Subscription {
        // No need to spawn a thread if the interests are empty.
        if interest.is_empty() {
            return Subscription(None);
        }
        let (sender, receiver) = oneshot::channel();
        let mut stream = Self::stream(interest)
            .map(Message::Preferences)
            .race(stream::once_future(receiver).map(|_| Message::Shutdown));
        async_rt::spawn_future(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Message::Preferences(preferences) => callback(preferences),
                    Message::Shutdown => break,
                }
            }
        });
        Subscription(Some(sender))
    }
}

enum Message {
    Preferences(Preferences),
    Shutdown,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn dropping_subscription_cancels_receiver() {
        let (sender, receiver) = oneshot::channel();
        let subscription = Subscription(Some(sender));
        drop(subscription);
        assert!(futures_lite::future::block_on(receiver).is_err());
    }

    #[test]
    fn empty_interest_does_not_start_subscription() {
        let subscription = Preferences::subscribe_impl(Interest::default(), |_| {
            panic!("empty interest must not invoke the callback");
        });
        assert!(subscription.0.is_none());
    }
}

#[cfg(all(test, target_os = "linux", feature = "reduced-motion"))]
#[path = "callback_private_bus_test.rs"]
mod private_bus_test;
