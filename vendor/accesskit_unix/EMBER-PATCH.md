# Ember local compatibility patch

Source: `accesskit_unix 0.21.1` from crates.io.
Archive SHA-256: `b016ca8db0ea0ea2ceff29a9d6240391492d960716aa471967c00e8cc8cb197c`.
Upstream revision: `f40dfc01a0c0e76de535969f82fb35e19513737d`.

The original manifest and license notices are retained. See
[the migration record](../../docs/linux-dbus-zbus6.md) for scope, runtime changes,
validation and limitations. Modified source/manifests carry a patch notice.
The reviewable delta is [the corresponding patch](../../docs/evidence/linux-dbus-patches/accesskit_unix.patch).
Crate-local Cargo.lock and extraction metadata are omitted; all fork tests and
dev dependencies use Ember's root Cargo.lock.
