# Public Block Search control proof

This isolated hosted-only fixture uses the unchanged installed production binary.
It supplements discovery; it does not replace the existing E03 raw/snapshot gate
or establish search correctness, performance, loading Return, Stop/Restart or
result navigation.

The f068 discovery artifact exposed a real named Block Search frame containing an
editable Entry, the three named scope toggles, and public Action/Component/Text
interfaces. Runtime node addresses and geometry are always rediscovered. The
scope state is Pressed bit 20, not Selected bit 23. Editor focus is bit 12.

The proof requires the unique picker under the same PID-owned top-level window,
all controls within its geometry and hierarchy, public Text equality and clipboard
byte readback. Each scope click action index is resolved from its current advertised
name; Each action must first observe its opposing source state in its own fresh tree.
Cmd then Out must produce reciprocal All-to-Cmd-to-Out Pressed-state transitions.
An already-target state fails before dispatch even if a no-op would return true.
Picker/editor/scope identities must remain identical across each operation; an
equivalent replacement tree cannot prove the original target responded. Public focus
must move from editor to Out and then back to editor, with scope/query unchanged.
A physical replacement query must then match both exact clipboard bytes and fresh
public editor Text. Action true or Enabled state alone never proves success.
The observed disabled-looking Latest button is intentionally not invoked.

A real foreground PTY collector first receives an exact physical PTYCONTROL
positive control on the same shell PID/TTY. A second idle collector then surrounds
all picker input and public operations. It must receive exactly zero bytes,
including its final drain. Final collector metadata is atomically renamed only
after the bytes artifact is written. Input guards reject a finished collector or leaked
bytes before the next challenge. The collector never executes received input.
It has a 120-second ceiling; the proof has a 90-second between-operation deadline,
8-second response-poll target and bounded discovery calls. A blocking traversal
can exceed those soft targets; the hosted job has a 4-minute outer ceiling.

No public operation is retried. Polling after it is read-only and bounded. Missing
focus-away response, unsupported interface/action, stale ownership, duplicated
controls, query change, or no-op action is a failure/unproven result, not a fallback
to guessed pointer positions, repeated toggle or private application hooks.

Artifacts preserve trees, screenshots, operation return/response records, raw PTY
positive and zero-byte captures, owner/window/shell identity, binary hash, and
result. Existing unconditional Smoke cleanup is retained. Activated-service
membership and exit are still unverified; no full cleanup acceptance is claimed.
A later separately reviewed cleanup-evidence stage must resolve that limitation.
