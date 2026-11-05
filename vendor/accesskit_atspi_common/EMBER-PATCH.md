# Ember local compatibility patch

Source: `accesskit_atspi_common 0.18.1` from crates.io.
Archive SHA-256: `1e8c61bee90b42a772d39d06a740207dc71a4e780004ace1db8d99fb1baaa954`.
Upstream revision: `f40dfc01a0c0e76de535969f82fb35e19513737d`.

The original manifest and license notices are retained. See
[the migration record](../../docs/linux-dbus-zbus6.md) for scope, runtime changes,
validation and limitations. Modified source/manifests carry a patch notice.
The reviewable delta is [the corresponding patch](../../docs/evidence/linux-dbus-patches/accesskit_atspi_common.patch).
Crate-local Cargo.lock and extraction metadata are omitted; all fork tests and
dev dependencies use Ember's root Cargo.lock.
