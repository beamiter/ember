# Recoverable native design checkpoint

This master checkpoint preserves the current native GUI reconstruction. It is not final 1:1 reference acceptance or a claim that all tests pass.

The UI source checkpoint is `a25e33eff1b808a9d12ab54477d3a328784eaef3`. The bundled core is published as `c0fcbd2e32a727c053c1e6e26261b81b5b89d66a` (same complete tree as the recorded local source `84c96ddda17f34091957aff19ea2004e85a418a2`); its source archive includes the required identity assets and has been checked against the committed core bytes. Documentation-only commits do not change that compiled UI source.

The spacing/accessibility release has native Block/Chat, exactly-once PTY, resize and fixed-boundary checks. Complete input/IME/stream/menu/Stop evidence retains its earlier 1895f98 build attribution; this is not a new full-suite result.

Structured Assistant/task/diff components use an explicitly labelled visual fixture until their production event binding is complete. Unavailable Attach/Image/Code/Apply/model actions remain visibly disabled. Native PTY and retained canvas/input ownership are preserved.

Original-reference typography, surfaces and cross-backend pixel acceptance remain open. Software headless checks do not certify physical desktop IME integration or hardware frame pacing.
