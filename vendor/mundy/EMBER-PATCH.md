# Ember local compatibility patch

Source: `mundy 0.2.3` from crates.io.
Archive SHA-256: `f32eb0db40f2df2bcfb05c93b8f73938d4c26ce9ac8881f1df0c8d3296921a73`.
Upstream revision: `608fe75c6a5eb82b14ac4539455c00ce01e774fb`.

The original manifest and license notices are retained. See
[the migration record](../../docs/linux-dbus-zbus6.md) for scope, runtime changes,
validation and limitations. Modified source/manifests carry a patch notice.
The reviewable delta is [the corresponding patch](../../docs/evidence/linux-dbus-patches/mundy.patch).
Crate-local Cargo.lock and extraction metadata are omitted; all fork tests and
dev dependencies use Ember's root Cargo.lock.
