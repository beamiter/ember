# Ember local compatibility patch

Source: `atspi-proxies 0.13.0` from crates.io.
Archive SHA-256: `2230e48787ed3eb4088996eab66a32ca20c0b67bbd4fd6cdfe79f04f1f04c9fc`.
Upstream revision: `f486755b9bf993613f63a779133aafc54925004a`.

The original manifest and license notices are retained. See
[the migration record](../../docs/linux-dbus-zbus6.md) for scope, runtime changes,
validation and limitations. Modified source/manifests carry a patch notice.
The reviewable delta is [the corresponding patch](../../docs/evidence/linux-dbus-patches/atspi-proxies.patch).
Crate-local Cargo.lock and extraction metadata are omitted; all fork tests and
dev dependencies use Ember's root Cargo.lock.
