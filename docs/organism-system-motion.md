# Organism Automatic and desktop motion preferences

Automatic preserves Ember's Calm fallback when no reduced-motion request is
available. A reduced-motion request selects Static. Explicit Full, Calm and
Static settings always win; neither the desktop setting nor the saved Ember
configuration is changed.

Ember uses mundy **0.2.3**, Apache-2.0, with only `async-io`, `callback` and
`reduced-motion` features. Its documented `Preferences::subscribe` API is
created on the UI thread, delivers initial state and subsequent changes, and
cancels when the window-owned subscription is dropped. No preference polling
or animation timer is added. Equal values do not request another repaint;
rapid events coalesce into the latest atomic state.

On Linux the dependency reads the desktop portal Settings interface and listens
to `SettingChanged`. It prefers `org.freedesktop.appearance/reduced-motion`,
falling back to `org.gnome.desktop.interface/enable-animations` where available.
Unavailable preferences retain Calm. A desktop without either exposed setting
cannot be inferred from unrelated UI options. Other supported native platforms
use the dependency's OS adapters; their runtime behavior is not established by
Linux-only tests.

A changed value requests an immediate existing egui repaint. Existing
PresentationPolicy and host scheduling then stop recurring animation in
Automatic+reduced. egui/eframe has no per-organism cancellation API: an already
queued repaint can still arrive once, but does not rearm animation. Existing
bounded input-retreat and completion-expiry repaints remain. Hidden, remote and
alternate-screen owners continue to request no organism wakes. Changes never
reset command generations, completion status, life state or ownership.

The settings preview receives the same preference snapshot. Explicit preview
controls and their bounded sequences retain their existing behavior.

## Shared policy follow-up

This is a temporary Ember host mapper, not a cross-terminal policy extraction.
`OrganismMotion` is defined by Ember's `src/config.rs`; the pinned
`beamiter/jterm_core` revision `d7dc25ef8d3a54c9126571589591b26f70854f18`
provides organism life/rendering/attention APIs but no system-motion resolver.
E12/R12 depend on coordinating a pure shared mapping API and the terminal
adapters. This patch changes neither that core revision nor the other terminals.

## Sources and verification boundary

- https://docs.rs/mundy/0.2.3/mundy/struct.Preferences.html
- https://docs.rs/crate/mundy/0.2.3/source/src/freedesktop/mod.rs
- https://docs.rs/crate/mundy/0.2.3/source/src/callback.rs
- https://docs.rs/crate/mundy/0.2.3/source/license.txt

Headless tests cover policy transitions, duplicate coalescing, explicit choices,
completion preservation and suppressed-owner wake behavior. A real desktop
portal event and native UI wake trace still require hosted native acceptance;
these unit tests are not evidence that such an OS event was delivered.
