//! Raw-copy bodies frozen before the shared inclusive-selection extraction.
//! Expected bytes retain the native profile's original bounds and whitespace.
use super::*;

impl TerminalState {
    fn frozen_raw_copy(&self, preserve_soft_spaces: bool) -> Option<String> {
        if self.projection_unavailable().is_some() {
            return None;
        }
        if let Some(selection) = &self.inline_text_selection {
            return (!self.is_alt_buffer_active())
                .then(|| selection.copy(preserve_soft_spaces))
                .flatten();
        }
        if self.projected_selection.is_some() {
            return self.copy_projected_selection();
        }
        self.selection.map(|sel| {
            let (start, end) = if sel.anchor <= sel.active {
                (sel.anchor, sel.active)
            } else {
                (sel.active, sel.anchor)
            };
            let mut result = String::new();
            let scrollback_len = self.scrollback.len();
            let grid_rows = self.vt.screen.grid.rows();
            let cols = self.vt.screen.grid.row_len();
            let total_rows = scrollback_len + grid_rows;

            let block = sel.mode == SelectionMode::Block;
            let last_row = end.0.min(total_rows.saturating_sub(1));
            for abs_row in start.0..=last_row {
                let (start_col, end_col) = if block {
                    // Copy exactly the rectangle painted by the selection
                    // overlay, including reverse and upward drags.
                    (
                        sel.anchor.1.min(sel.active.1),
                        sel.anchor.1.max(sel.active.1).min(cols.saturating_sub(1)),
                    )
                } else {
                    (
                        if abs_row == start.0 { start.1 } else { 0 },
                        if abs_row == end.0 {
                            end.1.min(cols.saturating_sub(1))
                        } else {
                            cols.saturating_sub(1)
                        },
                    )
                };

                // 行是否因到达行末被自动换行(软换行)。复制时软换行不应插入 \n,
                // 否则像 URL 这种被终端宽度截断的字符串会被切断成多段。
                let row_wrapped = if abs_row < scrollback_len {
                    self.scrollback[abs_row].is_wrapped
                } else {
                    let grid_row = abs_row - scrollback_len;
                    self.vt.screen
                        .grid
                        .row_wrapped
                        .get(grid_row)
                        .copied()
                        .unwrap_or(false)
                };

                let mut line_buf = String::new();
                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = self.scrollback[abs_row].decompress();
                    for cell in line.iter().take(end_col.saturating_add(1)).skip(start_col) {
                        if !cell.flags.wide_continuation() {
                            line_buf.push(cell.character);
                        }
                    }
                } else {
                    // Read from current grid
                    let grid_row = abs_row - scrollback_len;
                    if grid_row < grid_rows {
                        for col in start_col..=end_col {
                            let cell = self.vt.screen.grid.get(grid_row, col);
                            if !cell.flags.wide_continuation() {
                                line_buf.push(cell.character);
                            }
                        }
                    }
                }

                // 软换行(URL 等被终端宽度截断)拼接时去掉尾部填充空白,
                // 避免还原后的字符串里夹杂大段空格。
                if !preserve_soft_spaces && !block && row_wrapped && abs_row < last_row {
                    let trimmed_len = line_buf.trim_end_matches(' ').len();
                    line_buf.truncate(trimmed_len);
                }

                result.push_str(&line_buf);

                if abs_row < last_row && (block || !row_wrapped) {
                    result.push('\n');
                }
            }

            result
        })
    }
}

fn seeded() -> TerminalState {
    let mut t = TerminalState::new(6, 4);
    for (index, width) in [3, 9, 0, 6].into_iter().enumerate() {
        let mut cells = vec![TerminalCell::default(); width];
        for (col, cell) in cells.iter_mut().enumerate() {
            cell.character = if col + 2 >= width {
                ' '
            } else {
                char::from(b'a' + col as u8)
            };
        }
        if width >= 3 {
            cells[1].character = '界';
            cells[1].flags.set_wide(true);
            cells[2].flags.set_wide_continuation(true);
        }
        t.scrollback
            .push_back(ScrollbackLine::compress(&cells, index % 2 == 0));
    }
    for row in 0..4 {
        for (col, character) in ['a', '界', ' ', '\u{00a0}', ' ', ' ']
            .into_iter()
            .enumerate()
        {
            let cell = t.vt.screen.grid.get_mut(row, col);
            cell.character = character;
            cell.flags.set_wide(col == 1);
            cell.flags.set_wide_continuation(col == 2);
        }
        t.vt.screen.grid.row_wrapped[row] = row % 2 == 0;
    }
    t
}

#[test]
fn shared_raw_copy_matches_frozen_native_bytes() {
    let mut t = seeded();
    let total = t.scrollback.len() + t.vt.screen.grid.rows();
    let points: Vec<_> = [0, 1, t.scrollback.len(), total - 1, total, total + 1]
        .into_iter()
        .flat_map(|row| {
            [0, 1, 2, 5, 8, usize::MAX]
                .into_iter()
                .map(move |col| (row, col))
        })
        .collect();
    for &anchor in &points {
        for &active in &points {
            for block in [false, true] {
                t.selection = Some(Selection {
                    anchor,
                    active,
                    mode: if block {
                        SelectionMode::Block
                    } else {
                        SelectionMode::Normal
                    },
                });
                for preserve in [false, true] {
                    assert_eq!(
                        t.copy_displayed_selection_with_soft_spaces(preserve),
                        t.frozen_raw_copy(preserve),
                        "anchor={anchor:?} active={active:?} block={block}"
                    );
                }
            }
        }
    }
    t.selection = None;
    for preserve in [false, true] {
        assert_eq!(
            t.copy_displayed_selection_with_soft_spaces(preserve),
            t.frozen_raw_copy(preserve)
        );
    }
}

#[test]
fn shared_raw_copy_keeps_retention_and_resize_selection_contracts() {
    for cap in [1, 2, 4] {
        for reverse in [false, true] {
            for block in [false, true] {
                let mut t = TerminalState::new(6, 2);
                t.set_max_scrollback(cap);
                t.process_input(b"one\r\ntwo\r\nthree\r\nfour");
                let (anchor, active) = if reverse {
                    ((2, 4), (0, 1))
                } else {
                    ((0, 1), (2, 4))
                };
                t.selection = Some(Selection {
                    anchor,
                    active,
                    mode: if block {
                        SelectionMode::Block
                    } else {
                        SelectionMode::Normal
                    },
                });
                for bytes in [b"".as_slice(), b"\r\nnext", b"\r\nlast"] {
                    t.process_input(bytes);
                    for preserve in [false, true] {
                        assert_eq!(
                            t.copy_displayed_selection_with_soft_spaces(preserve),
                            t.frozen_raw_copy(preserve)
                        );
                    }
                }
                t.set_max_scrollback(1);
                for preserve in [false, true] {
                    assert_eq!(
                        t.copy_displayed_selection_with_soft_spaces(preserve),
                        t.frozen_raw_copy(preserve)
                    );
                }
                t.on_resize(3, 3);
                for preserve in [false, true] {
                    assert_eq!(
                        t.copy_displayed_selection_with_soft_spaces(preserve),
                        t.frozen_raw_copy(preserve)
                    );
                }
            }
        }
    }
}

#[test]
fn empty_raw_store_matches_frozen_copy_without_indexing_a_live_row() {
    let mut t = seeded();
    t.scrollback.clear();
    t.vt.screen.grid = TerminalGrid::new(0, 6);
    t.selection = Some(Selection {
        anchor: (0, 0),
        active: (9, 3),
        mode: SelectionMode::Normal,
    });
    for preserve in [false, true] {
        assert_eq!(
            t.copy_displayed_selection_with_soft_spaces(preserve),
            t.frozen_raw_copy(preserve)
        );
    }
}
