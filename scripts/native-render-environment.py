"""Record font/OCR file identities before native tests; never admit test pixels."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import stat
import subprocess


FONT_ROOTS = (Path('/usr/share/fonts'), Path('/usr/local/share/fonts'))
MODEL_ROOTS = (Path('/usr/share/tesseract-ocr'), Path('/usr/share/tessdata'))
MAX_IDENTITY_BYTES = 32 * 1024 * 1024


def command(args):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=10)
        if len(result.stdout) > 32768 or len(result.stderr) > 32768:
            raise ValueError('environment command output exceeded identity budget')
        if result.returncode:
            raise ValueError(f'{args[0]} exited {result.returncode}')
        return result.stdout
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f'{args[0]} unavailable: {error}') from error


def file_identity(path, roots):
    try:
        path = Path(path).resolve(strict=True)
        registered = any(path.is_relative_to(root.resolve()) for root in roots)
    except RuntimeError as error:
        raise ValueError('identity path has an invalid symbolic-link chain') from error
    if not registered:
        raise ValueError('identity file is outside the registered system-data roots')
    before = path.stat()
    if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_IDENTITY_BYTES:
        raise ValueError('identity file is not a bounded nonempty regular file')
    digest = hashlib.sha256()
    consumed = 0
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            consumed += len(block)
            if consumed > MAX_IDENTITY_BYTES:
                raise ValueError('identity file grew beyond the hashing budget')
            digest.update(block)
    after = path.stat()
    fields = ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns')
    if any(getattr(before, field) != getattr(after, field) for field in fields):
        raise ValueError('identity file changed while hashing')
    return {'path': str(path), 'bytes': after.st_size, 'sha256': digest.hexdigest()}


def font_identity(family, run=command, roots=FONT_ROOTS):
    rows = run(['fc-match', '-f', '%{family}\t%{style}\t%{file}\n', family]).splitlines()
    if len(rows) != 1 or len(fields := rows[0].split('\t')) != 3:
        raise ValueError('fontconfig did not return one complete font record')
    families, style, path = fields
    aliases = [item.strip() for item in families.split(',')]
    if family.casefold() not in [item.casefold() for item in aliases]:
        raise ValueError('requested family was substituted; its identity is unproven')
    return {'requested_family': family, 'resolved_families': aliases, 'style': style,
            'current_file': file_identity(path, roots)}


def english_model_identity(run=command, roots=MODEL_ROOTS):
    listing = run(['tesseract', '--list-langs'])
    header = re.search(r'^List of available languages in "([^"]+)"', listing, re.MULTILINE)
    if header is None or 'eng' not in listing.splitlines()[1:]:
        raise ValueError('Tesseract did not advertise an English model directory')
    return file_identity(Path(header.group(1)) / 'eng.traineddata', roots)


def observe(probe):
    try:
        return {'status': 'observed', 'value': probe()}
    except (OSError, ValueError) as error:
        return {'status': 'unavailable', 'error': str(error)}


def tesseract_version(run=command):
    lines = run(['tesseract', '--version']).splitlines()
    if not lines or not lines[0].strip():
        raise ValueError('Tesseract version output is empty')
    return lines[0]


def record():
    return {
        'schema': 1,
        'observed_at_utc': datetime.now(timezone.utc).isoformat(),
        'scope': 'Pre-native-test environment observation, not a pixel/IME acceptance result.',
        'limits': [
            'Hashes describe current resolved system files at this observation time.',
            'They do not prove bytes loaded by a later app or identify its GPU renderer.',
            'Actual per-case font paths and renderer remain in each native application log.',
            'Unavailable identities are retained; no substitute font or model is admitted.',
        ],
        'fonts': {family: observe(lambda family=family: font_identity(family))
                  for family in ('DejaVu Sans Mono', 'DejaVu Sans')},
        'tesseract_version': observe(tesseract_version),
        'english_model': observe(english_model_identity),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(record(), indent=2) + '\n')


if __name__ == '__main__':
    main()
