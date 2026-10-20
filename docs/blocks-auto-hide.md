# Blocks navigator auto-hide

The top Blocks navigator yields its space to alternate-screen applications and
recognized interactive editors, pagers, process monitors and agent CLIs. Returning
to the shell restores it unless it was manually hidden. The tab-bar toggle remains
available without adding another toolbar row. Hidden chrome reserves no height.

Simple shell launchers (`env`, variable assignments, `command`, `exec`, common
`sudo` options, `npx -y`, `bunx`, and `pnpm dlx/exec`) are recognized. Quoted paths
and option values are preserved. Codex `-p` / `--profile` stays interactive;
`codex exec`, Claude print, Gemini prompt, OpenCode run and Aider message commands
retain navigation for reviewing output. Help/version queries also keep it visible.

Compound shell commands and unknown wrapper options are deliberately not guessed;
alternate-screen detection still hides the bar. Foreground process detection is a
fallback when shell command metadata is absent, so wrapper/remote detection without
shell integration remains best-effort.

The standalone classification regression tests can run without GUI dependencies:

```sh
rustc --edition 2021 --test src/immersive_command.rs -o /tmp/blocks-auto-hide-tests
/tmp/blocks-auto-hide-tests
```
