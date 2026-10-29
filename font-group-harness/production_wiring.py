"""Check the real Files renderer's current-frame font ownership, not a literal.

This is a narrow Rust-source integration guard, not a Rust parser. Literals and
nested comments are opaque so documentation cannot satisfy executable wiring.
"""
import re

RAW_STRING = re.compile(r'(?:br|r)(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\.|[^'\\\n])'")
TOKEN = re.compile(r'[A-Za-z_]\w*|\d+(?:\.\d+)?|::|->|=>|&&|\|\|')


def _code(source):
    tokens = []
    i = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
        elif source.startswith('//', i):
            end = source.find('\n', i + 2)
            i = len(source) if end < 0 else end + 1
        elif source.startswith('/*', i):
            depth = 1
            i += 2
            while depth and i < len(source):
                if source.startswith('/*', i):
                    depth += 1
                    i += 2
                elif source.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            assert depth == 0, 'Unclosed Rust block comment'
        else:
            raw = RAW_STRING.match(source, i)
            char = CHAR_LITERAL.match(source, i)
            if raw:
                end_marker = '"' + raw[1]
                end = source.find(end_marker, raw.end())
                assert end >= 0, 'Unclosed Rust raw string'
                i = end + len(end_marker)
                tokens.append('LITERAL')
            elif source[i] == '"' or source.startswith('b"', i):
                i += 1 if source[i] == '"' else 2
                while i < len(source) and source[i] != '"':
                    i += 2 if source[i] == '\\' else 1
                assert i < len(source), 'Unclosed Rust string'
                i += 1
                tokens.append('LITERAL')
            elif char:
                i = char.end()
                tokens.append('LITERAL')
            else:
                word = TOKEN.match(source, i)
                token = word[0] if word else source[i]
                tokens.append(token)
                i += len(token)
    return ' '.join(tokens)


def _function(source, name):
    code = _code(source)
    starts = list(re.finditer(r'\bfn ' + re.escape(name) + r'\b', code))
    assert len(starts) == 1, f'Expected one real {name} function'
    start = code.index('{', starts[0].end())
    depth = 1
    end = start + 1
    while depth and end < len(code):
        depth += (code[end] == '{') - (code[end] == '}')
        end += 1
    assert depth == 0, f'Unclosed {name} function'
    return code[start + 1:end - 1]


def _top_level(code, match):
    prefix = code[:match.start()]
    return prefix.count('{') == prefix.count('}')


def verify_files_font_group_wiring(main, workspace):
    sidebar = _function(main, 'render_sidebar')
    choices = list(re.finditer(
        r'let (\w+) = if let Some \( (\w+) \) = (\w+) '
        r'\{ ([^{}]+) \} else \{ ([^{}]+) \} ;', sidebar))
    choices = [m for m in choices if ':: show_inspector_drawer (' in m[4]]
    assert len(choices) == 1 and _top_level(sidebar, choices[0]), 'One live Files dock/drawer owner required'
    choice = choices[0]
    response, drawer_rect = choice[1], choice[2]
    drawer = re.fullmatch(
        r'(\w+) :: show_inspector_drawer \( (\w+) , ' + re.escape(drawer_rect) +
        r' , LITERAL , (\w+) , & mut (\w+) ,? \)', choice[4].strip())
    assert drawer, 'Drawer must call the shared renderer on its actual rectangle'
    namespace, root, frame, renderer = drawer.groups()
    assert namespace == 'reference_workspace', 'Drawer must use the registered native helper'
    dock = re.fullmatch(
        r'\w+ \. resizable \( false \) \. exact_size \( \w+ \) '
        r'\. show_separator_line \( false \) \. frame \( ' + re.escape(frame) +
        r' \) \. show \( ' + re.escape(root) + r' , & mut ' + re.escape(renderer) + r' \)',
        choice[5].strip())
    assert dock, 'Dock and drawer must call the same single renderer'
    renderers = list(re.finditer(r'let mut ' + re.escape(renderer) + r' = \| ui : & mut egui :: Ui \| \{', sidebar))
    assert len(renderers) == 1 and _top_level(sidebar, renderers[0]), 'Shared renderer must be one live closure'
    rects = list(re.finditer(r'let (\w+) = ' + re.escape(response) + r' \. response \. rect ;', sidebar))
    assert len(rects) == 1 and _top_level(sidebar, rects[0]), 'Font rectangle must come from the actual owner response'
    rect = rects[0][1]
    registrations = list(re.finditer(
        r'self \. font_zoom \. register \( ' + re.escape(rect) + r' , ' +
        re.escape(response) + r' \. response \. layer_id , font_zoom :: FontGroup :: Ui ,? \) ;', sidebar))
    assert len(registrations) == 1 and _top_level(sidebar, registrations[0]), 'Files must register its actual response layer as UI'
    assert choice.end() < rects[0].start() < registrations[0].start(), 'Register the current, already-rendered surface'
    helper = _function(workspace, 'show_inspector_drawer')
    assert '. order ( egui :: Order :: Middle )' in helper, 'Drawer must retain its actual modeless layer'
    assert '. show ( & ctx , | ui | {' in helper, 'Drawer must return its Area response, not the parent layer'
    return {'owner_response': response, 'rectangle': rect, 'renderer': renderer,
            'dock_and_drawer': True, 'font_group': 'Ui', 'layer': 'owner response'}
