from pathlib import Path
import subprocess
BASE = '387ec628ecab655819be3b571666f070673590a1'
paths = ['src/vim9_ide.rs', 'scripts/native-vim9-buffer-smoke.py', '.github/workflows/ci.yml']
for path in paths:
    assert Path(path).read_bytes() == subprocess.check_output(['git', 'show', f'{BASE}:{path}']), path
p = Path(paths[0])
s = p.read_text()
s = s.replace('use crate::app::state::TerminalApp;', '#[path = "vim9_dialog_focus.rs"]\nmod dialog_focus;\n\nuse crate::app::state::TerminalApp;', 1)
old = '''                if dialog.needs_focus {
                    // Apply a category's pending focus before TextEdit reads
                    // this frame's input. The category controls are below the
                    // field, so their pointer click cannot steal it back.
                    ui.memory_mut(|memory| memory.request_focus(query_id));
                    dialog.needs_focus = false;
                }'''
new = '''                // Hidden sizing passes cannot own the query. Request focus
                // before TextEdit reads input, but keep it pending until visible.
                dialog_focus::request(ui, query_id, &mut dialog.needs_focus);'''
assert s.count(old) == 1
s = s.replace(old, new)
old = 'dialog.select_workbench_section(section.query);'
assert s.count(old) == 1
s = s.replace(old, old + '\n                                    dialog_focus::move_caret_to_end(ui.ctx(), edit.id, &dialog.query);')
p.write_text(s)
p = Path(paths[1])
s = p.read_text()
s = s.replace('    args = parser.parse_args()', '    parser.add_argument("--scenario", choices=("all", "palette"), default="all",\n                        help="Run the full native suite or the independent modal input gate")\n    args = parser.parse_args()', 1)
needle = '        report["command_palette_escape_focus"] = "passed"\n'
extra = '''        if args.scenario == "palette":
            # Cold/reopened modal input must not require a pointer click. Both
            # editor dialogs share the focus helper, but own distinct queries.
            for line, column in ((2, 3), (3, 1), (1, 1)):
                xdo("key", "ctrl+shift+g")
                xdo("type", "--clearmodifiers", "--delay", "0", f"{line}:{column}")
                xdo("key", "Return")
                wait_for(lambda: state().get("line") == line and state().get("column") == column,
                         f"keyboard-only Go to {line}:{column}", app)
                capture(f"goto-{line}-{column}")
                assert state().get("modified") == 0
                assert source.read_text(encoding="utf-8") == original
            report["goto_reopen_without_click"] = "passed"
            report["vim_state"] = state()
            command("qa")
            app.wait(timeout=10)
            assert app.returncode == 0
            report["normal_quit"] = "passed"
            (out / "result.json").write_text(json.dumps(report, indent=2) + "\\n")
            print(json.dumps(report, indent=2))
            return
'''
assert s.count(needle) == 1
s = s.replace(needle, needle + extra)
p.write_text(s)
p = Path(paths[2])
s = p.read_text()
needle = '      - name: Probe owned Block Search accessibility on isolated X11\n'
extra = '''      - name: Gate native Vim modal focus independently
        if: ${{ !cancelled() && steps.desktop-install.outcome == 'success' }}
        timeout-minutes: 3
        run: |
          xvfb-run -a -s '-screen 0 1600x1100x24' /usr/bin/python3 scripts/native-vim9-buffer-smoke.py \\
            --binary "$RUNNER_TEMP/ember-installed/bin/ember" --app ember --scenario palette \\
            --output native-smoke-artifacts/vim9-modal-focus

'''
assert s.count(needle) == 1
s = s.replace(needle, extra + needle)
p.write_text(s)
