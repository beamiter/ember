//! Frozen ember 2439756 native OSC 52 admission remains independent of the shared implementation.
//! Only storage paths changed; the original admission/queue branches remain host-local under cfg(test).
use super::*;

#[test]
fn shared_osc52_matches_original_native_admission_and_pending_effects() {
    let encode = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let mut requests = vec![
        "missing separator".into(),
        "c;".into(),
        "c;?".into(),
        "p;?".into(),
        "anything;?".into(),
    ];
    for text in [
        "hello",
        "\r\n\t好",
        "\x1b",
        "\0",
        "\u{fffd}",
        "\u{202e}",
        "\u{200b}",
        "\u{e0001}",
    ] {
        requests.push(format!("ignored;{}", encode(text)));
    }
    for count in [100 * 1024 - 1, 100 * 1024, 100 * 1024 + 1] {
        requests.push(format!("c;{}", encode(&"x".repeat(count))));
    }
    requests.extend([
        "c;!!!!".into(),
        "c;/w==".into(),
        "c;aA".into(),
        "c;aA==".into(),
        "c;Y Q==".into(),
        "c;YQ==;junk".into(),
    ]);
    for count in [100 * 1024 * 4 / 3 + 8, 100 * 1024 * 4 / 3 + 9] {
        requests.push(format!("c;{}", "x".repeat(count)));
    }
    requests.extend(std::iter::repeat_n("c;?".into(), 12));
    let mut original = TerminalState::new(8, 4);
    let mut shared = TerminalState::new(8, 4);
    for (i, request) in requests.iter().cycle().take(requests.len() * 2).enumerate() {
        let terminator: &'static [u8] = if i % 2 == 0 { b"\x07" } else { b"\x1b\\" };
        original.osc_reply_terminator = terminator;
        shared.osc_reply_terminator = terminator;
        original.original_handle_osc_52(request);
        shared.handle_osc_52(request);
        assert_eq!(shared.osc52, original.osc52, "request {i}");
        if i % 3 == 0 {
            assert_eq!(
                shared.take_osc52_clipboard_set(),
                original.take_osc52_clipboard_set()
            );
        }
        if i % 5 == 0 {
            assert_eq!(
                shared.take_osc52_clipboard_query(),
                original.take_osc52_clipboard_query()
            );
        }
    }
    assert_eq!(
        shared.take_osc52_clipboard_set(),
        original.take_osc52_clipboard_set()
    );
    while let Some(query) = original.take_osc52_clipboard_query() {
        assert_eq!(shared.take_osc52_clipboard_query(), Some(query));
    }
    assert_eq!(shared.take_osc52_clipboard_query(), None);
}
