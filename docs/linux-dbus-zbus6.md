# Linux D-Bus compatibility candidate

This patch ports Ember's existing Linux accessibility and desktop-preference
adapters to the released **zbus 6.2.1**. It does not disable AccessKit, reduced
motion, or any existing native gate. The production shared-core revision remains
`c1aebbe7b6ad4a8edb98582ea282d6e4d2ba6816`.

## Source and scope

The five local crates are unchanged release versions with explicit compatibility
patches, not new upstream releases:

- `mundy 0.2.3`
- `accesskit_unix 0.21.1`
- `accesskit_atspi_common 0.18.1`
- `atspi-common 0.13.0`
- `atspi-proxies 0.13.0`

[Source provenance](evidence/linux-dbus-vendor-provenance.json) records their exact
crates.io archive SHA-256 values, upstream commits, original manifest digests and
license files. Each directory retains `Cargo.toml.orig`. The AccessKit archives
omit their repository-root license texts; those are included from the exact
upstream commit recorded in the provenance. AT-SPI's repository-root rustfmt
configuration is also preserved from its exact release commit to avoid unrelated
formatting churn. The per-crate patch files under
[evidence/linux-dbus-patches](evidence/linux-dbus-patches) describe the source delta
against those archives. Crate-local lockfiles and Cargo extraction metadata are
omitted deliberately; the root lockfile includes all five crates' tests and dev
dependencies.

The `atspi 0.29.0` facade stays upstream. Its production feature scope remains
`proxies`; `connection`, `p2p`, and `atspi-connection` are not enabled or ported.
No Ember terminal, Mission, rendering, or PTY source is changed. Mundy's macOS,
Windows, Android and web implementation files remain byte-for-byte upstream.

The migration follows the upstream [zbus 6 guide](https://github.com/z-galaxy/zbus/blob/main/book/src/upgrading-to-6.md)
and [released 6.2.1 manifest](https://github.com/z-galaxy/zbus/blob/zbus-6.2.1/zbus/Cargo.toml).
The vendored manifests now require Rust 1.89, matching zbus; Ember's egui stack
already requires Rust 1.95.

## Protocol and runtime changes

- Wire/name types move to `zbus`/`zbus::names`; codecs move to `zbus::wire`.
  `Value`/`OwnedValue` become `Variant`/`OwnedVariant`.
- Builder conversion errors are checked at `build()`. AT-SPI retains its custom
  StateSet and InterfaceSet wire serialization, enum values, object references,
  and event signatures.
- zbus's merged error type has one conversion into `AtspiError`. Existing named
  variants remain available, but automatic zbus conversions use the unified
  codec/protocol error variant. Callers cannot distinguish the former crate
  origin of the merged error.
- Production lockstep macros are replaced by tests against the **same vendored
  AT-SPI XML**, using zbus 6 signatures and `zbus_xml 6.0.0`. Existing signature
  assertions remain; they are not hidden in an excluded dev graph.
- AccessKit retains its background thread. Its default runtime uses one outer,
  long-lived `zbus::block_on`, inside which both buses are constructed. The old
  manually ticked executors and task wrappers are removed. Tokio-only builds
  retain a separately owned current-thread Tokio runtime.
- `ObjectServer::at` returning `None` means insertion succeeded. Root embedding
  occurs only on that successful first registration, not on duplicate insertion.
- Both session and accessibility connections are explicitly watched for closure.
  A closed stream or transport ends the loop through one deactivation path and
  produces one diagnostic at the background-thread boundary. This only affects
  accessibility state. **Bus-loss reconnection is not implemented or claimed.**
  Screen-reader disable/enable on a live bus is a separate case.
- Mundy retains its callback thread, portal-over-GNOME preference precedence,
  cancellation, and optional owned Tokio runtime. Its legacy `async-io` feature
  now selects zbus's `default-rt`. The existing partial-interest GNOME filter
  behavior under an all-preferences build is unchanged; production Ember selects
  only reduced-motion.
- Rust 1.99 Clippy exposes existing nested-if lints in the now-local AccessKit
  common crate. Their let-chain rewrites preserve conditions and evaluation order;
  no warning suppression was added.

## Validation and CI

The workspace has `default-members = ["."]`. Existing application check/test/
Clippy coverage remains `-p ember --all-targets --all-features`, and release
builds remain application-only with `-p ember --release --all-features`; the new dedicated
Linux D-Bus job explicitly tests and lints every fork. AccessKit upstream rejects
`async-io + tokio`, so its supported default and Tokio-only configurations are
separate gates. Mundy's default, Tokio-only and additive runtime configurations
are tested separately.

The protocol/runtime test job first compiles with `--no-run`, then grants the
actual tests their 180-second timeout inside the unchanged 15-minute job budget.
Private-bus tests require `dbus-daemon`, create disposable runtime directories and
use explicit isolated addresses. The lifecycle fixture refuses an externally set
`AT_SPI_BUS_ADDRESS`, so it cannot silently use a desktop bus.

The candidate branch `candidate/zbus6` runs the same CI, UI and security workflows
as master. Master and pull-request triggers, permissions, existing native gates,
audit flags and advisory exceptions are retained.

Local validation on Linux with Rust 1.99.1:

- Full Ember all-target/all-feature `cargo check`: passed.
- Full Ember all-target/all-feature Clippy with `-D warnings`: passed.
- AT-SPI common/proxies: 1,141 unit tests and 9 doctests passed; the 31 upstream
  ignored doctests remain ignored. Core-only AT-SPI check and all-target Clippy
  passed.
- Mundy's initial 11 unit tests passed in default, Tokio-only and combined
  configurations; all 3 supported default-feature doctests also passed. The
  follow-up adds a twelfth real private-bus lifecycle test, compiled in every
  supported runtime configuration but not executed locally.
  These focused tests used debug info disabled; the application checks used their
  existing default profile. The cancellation unit test exercises the oneshot
  cancellation primitive and empty-interest no-op, not a live subscription's
  connection/thread cleanup. The exact production reduced-motion feature subset
  also passed all 11 unit tests. Its unit-only gate uses `--lib` because upstream
  doctest examples require the optional AccentColor feature; full doctests remain
  enabled in the default/all-preferences gate.
- AccessKit default and Tokio-only test binaries compiled. The initial three
  real private-bus tests did not reach their assertions locally: the execution
  environment rejected socket creation with `EPERM`, including an isolated retry.
  The follow-up active-tree tests and live Mundy subscription test are also
  compile-only locally; they are mandatory runtime gates in candidate CI. This
  is not a local protocol or lifecycle pass.
- `check-linux-dbus-stack.py` verifies a single active root zbus 6.2.1 graph, no
  obsolete split crates/lockstep in that graph, and provenance for the five
  audited fork test graphs. Unrelated historical `vendor/eframe/Cargo.lock` and
  `vendor/egui-winit/Cargo.lock` snapshots are not active resolution and remain
  untouched.
- `check-shared-core-pin.py` passed with the original immutable production pin.
- Local RustSec audit was not run because the pinned audit tool is not installed.
  Dependency absence is not an audit pass. Candidate CI runs the existing pinned
  cargo-audit 0.22.2 gate with no new ignores.

## Required before master

Candidate CI must pass the private-bus root-registration and lifecycle assertions
for default and Tokio runtimes. These cover exactly-once root embedding, duplicate
registration, real Parent `(so)`/role/state/interface results, error replies,
live-bus disabled→enabled toggles, session/a11y bus loss, and terminal callback
shutdown while activation is pending (`ActivationHandler` returns `None`).
Additional active-tree fixtures return a real `TreeUpdate`, export a window and
button, read their public AT-SPI children/name and execute one real Action. A FIFO
callback fence establishes that registration finished before bus loss. Both
session-bus and accessibility-bus loss must deactivate exactly once, clear the
AppContext tree, leave retained platform handles defunct and reject subsequent
remote actions. Session loss additionally observes the accessibility connection's
name-owner loss and verifies `NameHasOwner` is false. The tests pass the existing
callback channel into the loop rather than starting the process-global adapter
thread; the production path uses the very same channel as before.

The real Mundy fixture reexecutes its exact test under a disposable
`dbus-run-session`, drives public `Preferences::subscribe` through an initial Read
and a SettingChanged signal, then drops the subscription. Test-only observation
joins the actual callback thread after its outer `block_on` returns, including
thread-local runtime teardown. D-Bus `NameOwnerChanged` plus `NameHasOwner` confirms
that the subscriber connection is gone. A post-drop signal and bus round-trip
barrier must leave the callback channel disconnected with no queued callback.
The child exit status and explicit completion marker prevent a zero-test child
from passing. Bounded waits and an outer timeout fail hung teardown; there are no
sleep-based success conditions or ignored lifecycle tests. This covers the
subscription-owned thread and connection, not a claim that the process has no
other shared helper threads. Native acceptance must also pass:

- `scripts/test-organism-system-motion.py`: actual private-bus appearance, GNOME
  fallback and missing-preference behavior in Ember.
- Existing X11 AT-SPI public-tree/action/focus probes and interactions, including
  scope/focus changes, ownership and no accidental PTY input.
- Existing full desktop, Wayland geometry, IME, recovery, layout and application
  test gates on the exact candidate commit.

Compilation does not establish desktop accessibility, idle runtime behavior,
physical GPU behavior, non-Linux runtime behavior, or a real screen reader session.
Wayland AT-SPI smoke, repeated multi-window/adapter churn and process-wide
thread-growth observation remain additional runtime coverage to establish if they
are not exercised by the existing native jobs. The new active-tree and live-drop
fixtures must pass remotely before those narrower lifetime gaps are considered
closed. No upstream submission or master publication is part of
this candidate record.
