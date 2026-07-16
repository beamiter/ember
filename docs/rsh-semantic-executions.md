# rsh semantic execution protocol

This document is the integration contract shared by rsh and the jterm family.
It keeps the terminal as one continuous character grid; semantic command
records are an index over that grid, not a second block-based renderer.

## OSC 133 lifecycle

rsh emits the standard FinalTerm lifecycle on the PTY:

```text
OSC 133;A ST
prompt
OSC 133;B ST
interactive editor
OSC 133;C;id=ID;cmdline_url=COMMAND;cwd_url=CWD ST
rendered stdout/stderr
OSC 133;D;EXIT;id=ID;duration_ms=MILLIS;cwd_url=CWD_AFTER ST
```

`OSC` is `ESC ]`; `ST` may be BEL or `ESC \`. `A`, `B`, `C`, and `D`
retain their standard meanings:

- `A`: prompt begins.
- `B`: prompt is complete and the interactive command editor owns input.
- `C`: the command has been accepted and its output begins.
- `D`: execution has finished.

`id` is an ASCII, process-unique execution identifier. `cmdline_url` and
`cwd_url` are percent-encoded UTF-8. Delimiters, controls, `%`, and non-ASCII
bytes must be encoded. A consumer must reject malformed encodings, cap every
field before allocation, and ignore unknown keys. Command text omitted because
of a producer limit is not exact command text and must not be reconstructed
from an interactive editor repaint when an exact action such as rerun is
requested.
Such executions may still appear as metadata/output rows, but exact-command
copy, Fill, and Run again remain disabled.

Terminal actions are safe only after `B` and before `C`. `D` alone does not
make the editor ready: the consumer waits for the next `B`. A terminal must not
offer semantic fill/rerun while in the alternate screen.

## Shared execution journal

rsh writes lifecycle metadata and jterm writes normalized text captured from
the rendered range between `C` and `D`. Both append under an advisory lock:

```text
${XDG_STATE_HOME:-$HOME/.local/state}/rsh/executions.jsonl
${XDG_STATE_HOME:-$HOME/.local/state}/rsh/executions.lock
```

Every line is an independent JSON object. Readers skip malformed lines,
unknown versions, and unknown event types. Version 1 uses these envelopes:

```json
{"rsh_execution_version":1,"event":"start","id":"...","session_id":"...","seq":1,"command":"...","command_truncated":false,"cwd":"...","started_at_ms":0}
{"rsh_execution_version":1,"event":"finish","id":"...","exit_code":0,"duration_ms":12,"cwd_after":"...","ended_at_ms":12}
{"rsh_execution_version":1,"event":"output","id":"...","text":"...","truncated":false,"total_bytes":3,"captured_at_ms":12}
```

Events can arrive slightly out of order because the shell and terminal are
separate processes. Readers fold them by `id`, not by assuming that adjacent
JSON lines form a record. stdout and stderr share a PTY, so `text` is their
combined displayed transcript after terminal control sequences have been
applied. It intentionally represents what the user saw, not raw escape bytes.

The state directory and files are user-only (`0700` and `0600`). Output is
bounded to 256 KiB per execution with head and tail retained; the journal is
compacted at 32 MiB and retains the most recent 2,000 executions. Set
`RSH_EXECUTION_JOURNAL=0` in the environment to disable persistence while
leaving the live OSC timeline available. An absolute
`RSH_EXECUTION_JOURNAL_PATH` overrides the JSONL location for both rsh and
jterm; its sibling `executions.lock` remains the coordination lock.

## UI behavior

The Commands sidebar is a chronological index for the focused tab. A row can
jump to the existing terminal position, copy command/output, place a command
in the editor, or explicitly run it again. The terminal grid is never divided
into command blocks.

Fill and rerun require all of the following:

- rsh has emitted `B` for the current prompt;
- bracketed paste is enabled;
- the terminal is not in the alternate screen;
- no accepted input is waiting for PTY writer capacity.

Fill never appends Enter. Run again is a separate explicit action and sends
the sanitized bracketed-paste packet plus `CR` as one ordered write. Multiline
commands are not one-click rerun until a dedicated editor-control channel is
available.

AI providers receive persisted command output only under rsh's existing
extended-context policy: local providers can use it by default; cloud
providers require the user's explicit context-sharing opt-in.
