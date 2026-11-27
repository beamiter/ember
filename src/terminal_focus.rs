//! One logical focus owner, independent of how many PTYs continue parsing.
use crate::terminal::TerminalState;
use parking_lot::Mutex;

pub(crate) fn reconcile<'a>(
    terminals: impl Clone + Iterator<Item = &'a Mutex<TerminalState>>,
    owner: Option<usize>,
) -> bool {
    let mut changed = false;
    // Retire the old owner before admitting the new owner. An unchanged owner
    // keeps its state: no false/true pair on repaint or repeated OS focus.
    for (index, terminal) in terminals.clone().enumerate() {
        if Some(index) != owner {
            let mut terminal = terminal.lock();
            changed |= terminal.host_window_focused();
            terminal.set_host_window_focused(false);
        }
    }
    if let Some(terminal) = owner.and_then(|index| terminals.into_iter().nth(index)) {
        let mut terminal = terminal.lock();
        changed |= !terminal.host_window_focused();
        terminal.set_host_window_focused(true);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_switch_blur_hidden_negotiation_and_ris_use_actual_terminal_bytes() {
        let terminals = (0..3)
            .map(|_| Mutex::new(TerminalState::new(8, 2)))
            .collect::<Vec<_>>();
        reconcile(terminals.iter(), Some(0));
        for (index, t) in terminals.iter().enumerate() {
            let mut t = t.lock();
            t.process_input(b"\x1b[?1004h");
            assert_eq!(
                t.get_output(),
                if index == 0 {
                    b"\x1b[I".as_slice()
                } else {
                    b""
                }
            );
        }
        reconcile(terminals.iter(), Some(1));
        assert_eq!(terminals[0].lock().get_output(), b"\x1b[O");
        assert_eq!(terminals[1].lock().get_output(), b"\x1b[I");
        reconcile(terminals.iter(), Some(1));
        for t in &terminals {
            assert!(t.lock().get_output().is_empty());
        }
        terminals[1].lock().process_input(b"\x1bc\x1b[?1004h");
        assert_eq!(terminals[1].lock().get_output(), b"\x1b[I");
        reconcile(terminals.iter(), None);
        assert_eq!(terminals[1].lock().get_output(), b"\x1b[O");
        terminals[2].lock().process_input(b"\x1bc\x1b[?1004h");
        assert!(terminals[2].lock().get_output().is_empty());
        reconcile(terminals.iter(), None);
        for t in &terminals {
            assert!(t.lock().get_output().is_empty());
        }
    }
}
