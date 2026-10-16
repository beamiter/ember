# Isolated native motion-preference gate

The Tests job runs `scripts/test-organism-system-motion.py` after the ordinary
suite. It requires the distro `dbus`, `libglib2.0-bin`, `python3-dbus` and
`python3-gi` packages, and `/usr/bin/python3` to avoid virtualenv import drift.

The runner creates a new `dbus-run-session` process group for each of three
scenarios: the appearance reduced-motion key, the GNOME enable-animations
fallback, and unavailable initial preferences. It removes any inherited bus
address before launching. The service owns its mock portal name without queueing;
the test verifies a per-run synthetic nonce through a custom control method and
checks that the new bus address differs from the inherited address. Missing
setup fails the test. No real desktop setting is changed; no dconf/gsettings
commands or fallback to an ambient desktop bus are used.

The actual production `SystemMotion::connect` subscribes to the mock's public
portal `Read`/`SettingChanged` protocol. The ignored Linux binary test checks:

- Initial preference delivery, including the conservative unavailable fallback
- Repeated real signal toggles and duplicate-value repaint coalescing, after
  draining the first repaint so egui cannot conceal a duplicate request
- Explicit Full/Calm/Static policy and cadence preservation
- Existing bounded completion expiry followed by no recurring animation wake
- Disabled/hidden ownership, remote and alternate-screen sleep policy
- Retained life state, completion cursor, ownership and completion status
- Dropping the subscription while a signal may be in flight, followed by callback
  resource release and no later repaint

The egui Context is headless and records actual repaint callback requests. These
are real session-bus events and the actual production host policy, but this gate
is not a graphical whole-application CPU/wake trace or a non-Linux runtime test.
A previously queued settling repaint is permitted by the application contract.

Each event assertion has a five-second deadline; gdbus calls have a two-second
timeout. Each isolated scenario has a 180-second process deadline, with bounded
termination of only its owned process group. The test is ignored during ordinary
suite execution so an accidental invocation cannot discover a user's bus. The
explicit CI step runs it with `--ignored --exact`. A fresh create-only completion
marker tied to the run nonce is required, so a zero-test filter cannot silently pass.
Cleanup targets the owned process group even if its leader has exited, escalating
from TERM to KILL for surviving descendants after a bounded grace period.

Local source validation may typecheck and run a socket-free egui Context probe.
The real session-bus fixture must run on the hosted runner, never by working
around an executor's denied local socket policy.
