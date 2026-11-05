# Ember local compatibility patch

Source: `atspi-common 0.13.0` from crates.io.
Archive SHA-256: `20c5617155740c98003016429ad13fe43ce7a77b007479350a9f8bf95a29f63d`.
Upstream revision: `f486755b9bf993613f63a779133aafc54925004a`.

The original manifest and license notices are retained. See
[the migration record](../../docs/linux-dbus-zbus6.md) for scope, runtime changes,
validation and limitations. Modified source/manifests carry a patch notice.
The reviewable delta is [the corresponding patch](../../docs/evidence/linux-dbus-patches/atspi-common.patch).
Crate-local Cargo.lock and extraction metadata are omitted; all fork tests and
dev dependencies use Ember's root Cargo.lock.
