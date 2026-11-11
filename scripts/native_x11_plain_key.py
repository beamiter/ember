"""Resolve a verified plain F5 for xdotool's numeric-keycode input path.

Old libxdo mistakes explicit Alt->Level1 XKB entries for required modifiers.
Numeric keycodes bypass that inference. They use group zero in libxdo, so fail
closed on another active group rather than silently changing the test's layout.
This module reads X11 state only; it never releases keys or edits a keymap.
"""
import ctypes as C
import json
from pathlib import Path
import subprocess
import sys

QUERY_TIMEOUT_SECONDS = 4

F5_KEYSYM = 0xFFC2  # X11 protocol keysym, not a layout-dependent keycode.


class XkbState(C.Structure):
    _fields_ = [('group', C.c_ubyte), ('locked_group', C.c_ubyte),
                ('base_group', C.c_ushort), ('latched_group', C.c_ushort),
                ('mods', C.c_ubyte), ('base_mods', C.c_ubyte),
                ('latched_mods', C.c_ubyte), ('locked_mods', C.c_ubyte),
                ('compat_state', C.c_ubyte), ('grab_mods', C.c_ubyte),
                ('compat_grab_mods', C.c_ubyte), ('lookup_mods', C.c_ubyte),
                ('compat_lookup_mods', C.c_ubyte), ('ptr_buttons', C.c_ushort)]


class ModifierMap(C.Structure):
    _fields_ = [('max_keypermod', C.c_int), ('modifiermap', C.POINTER(C.c_ubyte))]


def select_plain_f5(rows, modifier_codes, active_modifier_codes, group, mods):
    """Rows contain (keycode, group-zero Level1 symbol, unmodified lookup symbol)."""
    if group != 0:
        raise RuntimeError('Plain F5 requires active XKB group zero for numeric xdotool input')
    if mods or active_modifier_codes:
        raise RuntimeError('Plain F5 refused: active X11 modifiers must not be cleared or ignored')
    candidates = [code for code, level1, plain in rows
                  if 8 <= code <= 255 and code not in modifier_codes
                  and level1 == F5_KEYSYM and plain == F5_KEYSYM]
    if len(candidates) != 1:
        raise RuntimeError('Plain F5 requires exactly one verified unmodified X11 keycode')
    return candidates[0]


def _query_plain_f5(display_name):
    x = C.CDLL('libX11.so.6')
    signatures = {
        'XOpenDisplay': ([C.c_char_p], C.c_void_p),
        'XCloseDisplay': ([C.c_void_p], C.c_int),
        'XDisplayKeycodes': ([C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)], C.c_int),
        'XGetModifierMapping': ([C.c_void_p], C.POINTER(ModifierMap)),
        'XFreeModifiermap': ([C.POINTER(ModifierMap)], C.c_int),
        'XQueryKeymap': ([C.c_void_p, C.POINTER(C.c_char)], C.c_int),
        'XkbGetState': ([C.c_void_p, C.c_uint, C.POINTER(XkbState)], C.c_int),
        'XkbKeycodeToKeysym': ([C.c_void_p, C.c_ubyte, C.c_int, C.c_int], C.c_ulong),
        'XkbLookupKeySym': ([C.c_void_p, C.c_ubyte, C.c_uint, C.POINTER(C.c_uint), C.POINTER(C.c_ulong)], C.c_int),
    }
    for name, (args, result) in signatures.items():
        function = getattr(x, name)
        function.argtypes, function.restype = args, result
    display = x.XOpenDisplay(display_name.encode())
    if not display:
        raise RuntimeError('Cannot open fixture X11 display for plain F5 verification')
    mapping = None
    try:
        state = XkbState()
        if x.XkbGetState(display, 0x0100, C.byref(state)) != 0:  # XkbUseCoreKbd
            raise RuntimeError('Cannot read fixture XKB modifier state')
        low, high = C.c_int(), C.c_int()
        x.XDisplayKeycodes(display, C.byref(low), C.byref(high))
        if not 8 <= low.value <= high.value <= 255:
            raise RuntimeError('Invalid fixture X11 keycode range')
        mapping = x.XGetModifierMapping(display)
        if not mapping or not 0 < mapping.contents.max_keypermod <= 256:
            raise RuntimeError('Cannot read fixture X11 modifier map')
        modifier_codes = {mapping.contents.modifiermap[i]
                          for i in range(8 * mapping.contents.max_keypermod)} - {0}
        pressed = C.create_string_buffer(32)
        if not x.XQueryKeymap(display, pressed):
            raise RuntimeError('Cannot read fixture X11 pressed-key state')
        active = {code for code in modifier_codes if pressed.raw[code // 8] & (1 << (code % 8))}
        rows = []
        for code in range(low.value, high.value + 1):
            level1 = x.XkbKeycodeToKeysym(display, code, 0, 0)
            if level1 != F5_KEYSYM:
                continue
            consumed, plain = C.c_uint(), C.c_ulong()
            if x.XkbLookupKeySym(display, code, 0, C.byref(consumed), C.byref(plain)):
                rows.append((code, level1, plain.value))
        mods = state.mods | state.base_mods | state.latched_mods | state.locked_mods
        code = select_plain_f5(rows, modifier_codes, active, state.group, mods)
        return {'keycode': code, 'keysym': F5_KEYSYM, 'group': 0,
                'modifiers': 0, 'method': 'XKB Level1 and zero-modifier lookup; numeric xdotool'}
    finally:
        if mapping:
            x.XFreeModifiermap(mapping)
        x.XCloseDisplay(display)


def resolve_plain_f5(display_name):
    """Bound all synchronous Xlib calls, including open/close, in a child."""
    try:
        result = subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), display_name],
            capture_output=True, text=True, timeout=QUERY_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise RuntimeError('Plain F5 X11 verification timed out') from error
    if result.returncode:
        raise RuntimeError('Plain F5 X11 verification failed: ' + result.stderr[-1024:])
    return json.loads(result.stdout)


if __name__ == '__main__':
    try:
        print(json.dumps(_query_plain_f5(sys.argv[1])))
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
