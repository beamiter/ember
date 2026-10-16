#!/usr/bin/env python3
"""Installed-app recovery acceptance through real desktop input and screenshots.

Run separately from the general desktop gate. Imports its compositor and OCR
helpers without replacing them. Uses only isolated synthetic session metadata.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import time

spec = importlib.util.spec_from_file_location('desktop_smoke', Path(__file__).with_name('native-desktop-smoke.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


def click_phrase(smoke, phrase, label):
    rows = smoke.words(label)
    wanted = phrase.split()
    matches = []
    for index in range(len(rows) - len(wanted) + 1):
        group = rows[index:index + len(wanted)]
        if [row['text'].strip() for row in group] != wanted:
            continue
        # A button label must occupy one OCR row. Reject unrelated phrases
        # joined across lines, and do not confuse the explanatory paragraph.
        tops = [int(row['top']) for row in group]
        if max(tops) - min(tops) <= 5:
            matches.append(group)
    assert len(matches) == 1, (phrase, matches)
    row = matches[0][0]
    smoke.click(int(row['left']) + int(row['width']) // 2,
                int(row['top']) + int(row['height']) // 2)


def exercise(smoke, case):
    count = 67 if case == 'over-limit' else 2
    value = {'version': 4, 'sessions': [dict(name=f'synthetic-{i}', tags=[], session_id=f'recovery-{i}') for i in range(count)]}
    if case == 'corrupt-layout':
        value['tabs'] = [{'root': {'kind': 'unknown'}}]
    if case == 'invalid-cwd':
        value['sessions'][1]['cwd'] = str(smoke.out / 'missing-directory')
    source = smoke.config.parent / 'session_history.json'
    original = json.dumps(value).encode()
    source.write_bytes(original)
    source.chmod(0o600)
    smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\ninitial_width = 1100\ninitial_height = 800\nrestore_session = true\n')
    smoke.app = smoke.spawn([str(smoke.binary)], 'ember')
    driver.wait_for(smoke.app_window, 'Ember recovery startup')
    time.sleep(2)
    rows = smoke.words('startup')
    text = ' '.join(row['text'] for row in rows)
    (smoke.out / 'startup-ocr.txt').write_text(text)
    assert smoke.app.poll() is None
    if case == 'normal':
        assert 'recovery summary' not in text.lower(), text
        smoke.command('quiet-startup-usable')
        return {'normal_startup_quiet': True}
    assert 'recovery summary' in text.lower(), text
    if case == 'over-limit':
        assert '64 session records retained; 3 skipped' in text, text
    elif case == 'corrupt-layout':
        assert 'pane layout adjusted' in text, text
    else:
        assert 'Session 2:' in text and 'directory invalid or unavailable' in text, text
    # Interact with a visible terminal region outside the nonmodal window.
    smoke.click(900, 680)
    smoke.command('summary-nonmodal-usable')
    smoke.click_word('Dismiss', 'before-dismiss')
    smoke.click_word('details', 'reopen-summary')
    click_phrase(smoke, 'Recheck original metadata', 'before-recheck')
    after = ' '.join(row['text'] for row in smoke.words('after-recheck'))
    assert 'Live sessions unchanged' in after, after
    # Export requires an explicit UI path and click. Use the entry's hint to
    # find it, never an application-private action or command channel.
    smoke.click_word('absolute', 'export-path')
    export = smoke.out / 'original-export.json'
    smoke.text(str(export))
    smoke.click_word('locally', 'export-button')
    driver.wait_for(export.exists, 'user-initiated local export')
    assert export.read_bytes() == original
    assert export.stat().st_mode & 0o777 == 0o600
    return {'summary_causes': True, 'nonmodal_terminal_input': True, 'dismiss_reopen': True,
            'read_only_recheck': True, 'original_export_sha256': hashlib.sha256(original).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--case', choices=['normal', 'corrupt-layout', 'over-limit', 'invalid-cwd'], required=True)
    args = parser.parse_args()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    smoke = driver.Smoke(args.binary.resolve(strict=True), args.backend, out)
    result = {'case': args.case, 'backend': args.backend, 'binary_sha256': hashlib.sha256(smoke.binary.read_bytes()).hexdigest()}
    try:
        smoke.start_desktop()
        result.update(exercise(smoke, args.case), status='pass')
    except Exception as error:
        result.update(status='fail', error=str(error))
        try: smoke.screenshot('failure')
        except Exception: pass
        raise
    finally:
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        smoke.close()


if __name__ == '__main__':
    main()
