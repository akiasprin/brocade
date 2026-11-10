use crate::HISTORY_BYTES;
use std::{collections::VecDeque, num::NonZeroU16};

#[derive(Default)]
pub(crate) struct History {
    rows: VecDeque<Vec<u8>>,
    bytes: usize,
    truncated: bool,
    capture: vt100::capture::BasicFormattedCaptureState,
    scrolled_rows: u64,
}

impl vt100::Callbacks for History {
    fn on_scroll(&mut self, row: vt100::capture::RowContents<'_>, alternate: bool) {
        if alternate {
            return;
        }
        self.scrolled_rows = self.scrolled_rows.saturating_add(1);
        // Capture the soft-wrap boundary independently of SGR state. Each retained
        // row must restore its own attributes after older rows have been evicted.
        let mut boundary = String::new();
        row.write_formatted_basic(&mut boundary, &mut self.capture)
            .expect("formatting a terminal row into String cannot fail");
        let mut text = if boundary.starts_with('\n') {
            String::from("\r\n\x1b[m")
        } else {
            String::from("\x1b[m")
        };
        row.write_formatted_basic(&mut text, &mut Default::default())
            .expect("formatting a terminal row into String cannot fail");
        let data = text.into_bytes();
        while self.bytes + data.len() > HISTORY_BYTES {
            let Some(old) = self.rows.pop_front() else {
                break;
            };
            self.bytes -= old.len();
            self.truncated = true;
        }
        if data.len() <= HISTORY_BYTES {
            self.bytes += data.len();
            self.rows.push_back(data);
        } else {
            self.truncated = true;
        }
    }
}

pub(crate) struct Screen {
    parser: vt100::Parser<History>,
    escape: Vec<u8>,
    escape_string: bool,
    string_escaped: bool,
    utf8: Vec<u8>,
}
impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                NonZeroU16::new(rows).expect("validated rows"),
                NonZeroU16::new(cols).expect("validated columns"),
                0,
                History::default(),
            ),
            escape: Vec::new(),
            escape_string: false,
            string_escaped: false,
            utf8: Vec::new(),
        }
    }
    pub fn process(&mut self, data: &[u8]) {
        for &byte in data {
            match byte {
                0xc2..=0xf4 => {
                    self.utf8.clear();
                    self.utf8.push(byte);
                }
                0x80..=0xbf if !self.utf8.is_empty() => {
                    self.utf8.push(byte);
                    let needed = match self.utf8[0] {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    if self.utf8.len() >= needed {
                        self.utf8.clear();
                    }
                }
                _ => self.utf8.clear(),
            }
            if byte == b'J' && self.escape == b"\x1b[3" {
                *self.parser.callbacks_mut() = History::default();
            }
            self.track_escape(byte);
            self.parser.process(&[byte]);
        }
    }
    fn track_escape(&mut self, byte: u8) {
        if self.escape_string {
            if matches!(byte, 0x18 | 0x1a)
                || (byte == 7 && self.escape == b"\x1b]")
                || (self.string_escaped && byte == b'\\')
            {
                self.escape.clear();
                self.escape_string = false;
                self.string_escaped = false;
            } else if self.string_escaped {
                self.escape_string = false;
                self.string_escaped = false;
                self.escape = vec![27];
                self.track_escape(byte);
            } else {
                self.string_escaped = byte == 27;
            }
            return;
        }
        if byte == 27 {
            self.escape.clear();
            self.escape.push(byte);
            return;
        }
        if self.escape.is_empty() {
            return;
        }
        if matches!(byte, 0x18 | 0x1a) {
            self.escape.clear();
            return;
        }
        if self.escape.len() == 1 {
            self.escape.push(byte);
            match byte {
                b']' | b'P' | b'X' | b'^' | b'_' => self.escape_string = true,
                b'[' | 0x20..=0x2f => {}
                _ => self.escape.clear(),
            }
            return;
        }
        if self.escape.len() < 128 {
            self.escape.push(byte);
        }
        let final_byte = if self.escape.get(1) == Some(&b'[') {
            0x40..=0x7e
        } else {
            0x30..=0x7e
        };
        if final_byte.contains(&byte) {
            self.escape.clear();
        }
    }
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.size() == (cols, rows) {
            return;
        }
        self.reflow(cols, rows);
    }

    fn reflow(&mut self, cols: u16, rows: u16) {
        let alternate = self.parser.screen().alternate_screen();
        let mut primary = vt100::Parser::default();
        *primary.screen_mut() = self.parser.screen().clone();
        if alternate {
            primary.process(b"\x1b[?1049l");
        }
        let normal = primary.screen();
        let mut saved = vt100::Parser::default();
        *saved.screen_mut() = normal.clone();
        saved.process(b"\x1b8");
        let positions = [normal.cursor_position(), saved.screen().cursor_position()];
        let old_cols = normal.size().1.get();
        let last_used = (0..normal.size().0.get())
            .rev()
            .find(|&row| (0..old_cols).any(|col| normal.cell(row, col).is_some_and(cell_used)))
            .unwrap_or(0)
            .max(positions[0].0)
            .max(positions[1].0);
        let mut rebuilt = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(rows).expect("validated rows"),
            NonZeroU16::new(cols).expect("validated columns"),
            0,
            History::default(),
        );
        // Retain terminal modes and the alternate grid; only the normal grid reflows.
        *rebuilt.screen_mut() = normal.clone();
        rebuilt.set_size(
            NonZeroU16::new(rows).expect("validated rows"),
            NonZeroU16::new(cols).expect("validated columns"),
        );
        *rebuilt.callbacks_mut() = History::default();
        rebuilt.callbacks_mut().truncated = self.truncated();
        rebuilt.process(b"\x1b[m\x1b[r\x1b[H\x1b[2J");
        for part in &self.parser.callbacks().rows {
            rebuilt.process(part);
        }
        let mut continuation = String::new();
        normal
            .row_contents(0)
            .expect("normal screen has at least one row")
            .write_formatted_basic(
                &mut continuation,
                &mut self.parser.callbacks().capture.clone(),
            )
            .expect("formatting terminal contents into String cannot fail");
        if continuation.starts_with('\n') {
            rebuilt.process(b"\r\n");
        }
        rebuilt.process(b"\x1b[m");
        let mut anchors = [(0, 0); 2];
        for row in 0..=last_used {
            let mut part = String::new();
            normal
                .row_contents(row)
                .expect("visible row exists")
                .write_formatted_basic(&mut part, &mut Default::default())
                .expect("formatting terminal contents into String cannot fail");
            rebuilt.process(b"\x1b[m");
            if row > 0 && !normal.row_wrapped(row - 1) {
                rebuilt.process(b"\r\n");
            }
            let mut at = 0;
            let mut order = [0, 1];
            order.sort_by_key(|&i| positions[i].1);
            let printed_cols = (0..old_cols)
                .rev()
                .find(|&col| normal.cell(row, col).is_some_and(cell_used))
                .map_or(0, |col| col + 1);
            let mut padding = 0;
            for index in order {
                if positions[index].0 != row {
                    continue;
                }
                let col = positions[index].1;
                let plain_bytes = (0..col.min(printed_cols))
                    .filter_map(|col| normal.cell(row, col))
                    .filter(|cell| !cell.is_wide_continuation() && !cell.is_wrap_padding())
                    .map(|cell| {
                        if cell.has_contents() {
                            cell.contents().len()
                        } else {
                            1
                        }
                    })
                    .sum();
                let end = capture_prefix_end(&part, plain_bytes);
                rebuilt.process(&part.as_bytes()[at..end]);
                at = end;
                let needed = col.saturating_sub(printed_cols);
                if needed > padding {
                    rebuilt.process(" ".repeat(usize::from(needed - padding)).as_bytes());
                    padding = needed;
                }
                let (row, col) = rebuilt.screen().cursor_position();
                anchors[index] = (rebuilt.callbacks().scrolled_rows + u64::from(row), col);
            }
            rebuilt.process(&part.as_bytes()[at..]);
        }
        move_cursor(&mut rebuilt, anchors[1]);
        rebuilt.process(saved.screen().attributes_formatted().as_bytes());
        rebuilt.process(b"\x1b7");
        move_cursor(&mut rebuilt, anchors[0]);
        rebuilt.process(normal.attributes_formatted().as_bytes());
        if alternate {
            // Switching with 47 preserves the resized alternate grid and primary cursor.
            rebuilt.process(b"\x1b[?47h");
            rebuilt.process(self.parser.screen().attributes_formatted().as_bytes());
        }
        rebuilt.process(self.parser.screen().input_mode_formatted().as_bytes());
        for part in self.pending_sequence() {
            rebuilt.process(&part);
        }
        self.parser = rebuilt;
    }

    fn pending_sequence(&self) -> Vec<Vec<u8>> {
        let mut parts = Vec::new();
        if !self.escape.is_empty() {
            let mut pending = self.escape.clone();
            if self.string_escaped {
                pending.push(27);
            }
            parts.push(pending);
        }
        if !self.utf8.is_empty() {
            parts.push(self.utf8.clone());
        }
        parts
    }
    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        (cols.get(), rows.get())
    }
    pub fn history_bytes(&self) -> usize {
        self.parser.callbacks().bytes
    }
    pub fn truncated(&self) -> bool {
        self.parser.callbacks().truncated
    }
    pub fn snapshot_current(&self) -> VecDeque<Vec<u8>> {
        self.snapshot_with_history(false)
    }
    pub fn snapshot(&self) -> VecDeque<Vec<u8>> {
        self.snapshot_with_history(true)
    }
    fn snapshot_with_history(&self, history: bool) -> VecDeque<Vec<u8>> {
        let mut chunks = VecDeque::new();
        let alternate = self.parser.screen().alternate_screen();
        let mut primary = vt100::Parser::default();
        *primary.screen_mut() = self.parser.screen().clone();
        if alternate {
            primary.process(b"\x1b[?1049l");
        }
        let normal = primary.screen();
        if history {
            chunks.push_back(b"\x1bc".to_vec());
            chunks.extend(self.parser.callbacks().rows.iter().cloned());
            // One continuous basic capture preserves soft-wrap boundaries where
            // retained history meets the visible viewport.
            let mut visible = String::new();
            normal
                .write_contents_formatted_basic(
                    &mut visible,
                    vt100::capture::BasicFormattedCaptureRange::Full(
                        &mut self.parser.callbacks().capture.clone(),
                    ),
                )
                .expect("formatting terminal contents into String cannot fail");
            chunks.push_back(visible.replace('\n', "\r\n").into_bytes());
        } else {
            // Recover just the viewport without clearing previously delivered history.
            chunks.push_back(b"\x1b[?1049l".to_vec());
            chunks.push_back(normal.state_formatted().into_bytes());
        }
        let mut saved = vt100::Parser::default();
        *saved.screen_mut() = normal.clone();
        saved.process(b"\x1b8");
        chunks.push_back(saved.screen().cursor_state_formatted().into_bytes());
        chunks.push_back(saved.screen().attributes_formatted().into_bytes());
        chunks.push_back(b"\x1b7".to_vec());
        chunks.push_back(normal.cursor_state_formatted().into_bytes());
        chunks.push_back(normal.attributes_formatted().into_bytes());
        if alternate {
            chunks.push_back(b"\x1b[?1049h".to_vec());
            chunks.push_back(self.parser.screen().state_formatted().into_bytes());
        } else {
            chunks.push_back(normal.input_mode_formatted().into_bytes());
        }
        chunks.extend(self.pending_sequence());
        chunks
    }
}

fn capture_prefix_end(capture: &str, mut bytes: usize) -> usize {
    if bytes == 0 {
        return 0;
    }
    for (start, plain) in vt100::capture::basic_formatted_to_plain_indexed(capture) {
        if bytes <= plain.len() {
            return start + bytes;
        }
        bytes -= plain.len();
    }
    capture.len()
}

fn cell_used(cell: &vt100::Cell) -> bool {
    if cell.is_wrap_padding() {
        return false;
    }
    cell.has_contents()
        || cell.is_wide_continuation()
        || cell.fgcolor() != vt100::Color::Default
        || cell.bgcolor() != vt100::Color::Default
        || cell.bold()
        || cell.dim()
        || cell.italic()
        || cell.underline()
        || cell.inverse()
}

fn move_cursor(parser: &mut vt100::Parser<History>, anchor: (u64, u16)) {
    let (rows, cols) = parser.screen().size();
    let row = anchor
        .0
        .saturating_sub(parser.callbacks().scrolled_rows)
        .min(u64::from(rows.get() - 1)) as u16;
    if anchor.1 < cols.get() {
        parser.process(format!("\x1b[{};{}H", row + 1, anchor.1 + 1).as_bytes());
    } else {
        // CUP clamps to the last cell. Redraw that cell to restore deferred wrapping.
        let last = cols.get() - 1;
        let start = if parser
            .screen()
            .cell(row, last)
            .is_some_and(vt100::Cell::is_wide_continuation)
        {
            last - 1
        } else {
            last
        };
        parser.process(format!("\x1b[{};{}H", row + 1, start + 1).as_bytes());
        let tail = parser
            .screen()
            .rows_formatted(start, cols.get() - start)
            .nth(usize::from(row))
            .expect("cursor row is inside the screen");
        parser.process(tail.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mirror(screen: &Screen) -> vt100::Parser {
        let (cols, rows) = screen.size();
        let mut mirror = vt100::Parser::new(
            NonZeroU16::new(rows).unwrap(),
            NonZeroU16::new(cols).unwrap(),
            4096,
        );
        for part in screen.snapshot() {
            mirror.process(&part);
        }
        mirror
    }
    fn all_text(parser: &vt100::Parser) -> String {
        let mut capture = String::new();
        parser
            .screen()
            .write_contents_formatted_basic(
                &mut capture,
                vt100::capture::BasicFormattedCaptureRange::Full(&mut Default::default()),
            )
            .unwrap();
        vt100::capture::basic_formatted_to_plain(&capture)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }
    #[test]
    fn reflow_preserves_unicode_colors_cursor_and_pending_wrap() {
        for text in [
            "BEGIN-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-END".to_owned(),
            format!("{}界e\u{301}{}", "x".repeat(79), "🙂".repeat(12)),
            "x".repeat(80),
            "❤\u{fe0f}".repeat(50),
        ] {
            let mut screen = Screen::new(80, 24);
            screen.process(b"\x1b[31m\x1b[?2004h");
            screen.process(text.as_bytes());
            let mut expected = mirror(&screen);
            for cols in [30, 5, 80] {
                screen.resize(cols, 24);
            }
            let mut actual = mirror(&screen);
            assert_eq!(actual.screen().contents(), expected.screen().contents());
            assert_eq!(
                actual.screen().cursor_position(),
                expected.screen().cursor_position()
            );
            assert_eq!(actual.screen().fgcolor(), expected.screen().fgcolor());
            assert!(actual.screen().bracketed_paste());
            screen.process(b"Z");
            actual.process(b"Z");
            expected.process(b"Z");
            assert_eq!(actual.screen().contents(), expected.screen().contents());
            assert_eq!(
                screen.parser.screen().contents(),
                expected.screen().contents()
            );
        }
    }
    #[test]
    fn reflow_preserves_text_across_the_history_boundary() {
        let mut screen = Screen::new(80, 4);
        let text = format!("BEGIN-{}-END\r\nNEXT", "界abc".repeat(200));
        screen.process(text.as_bytes());
        let before = all_text(&mirror(&screen));
        for cols in [30, 7, 80] {
            screen.resize(cols, 4);
        }
        let after = all_text(&mirror(&screen));
        assert_eq!(after, before);
        assert!(screen.history_bytes() <= HISTORY_BYTES);
    }
    #[test]
    fn resizing_alternate_screen_preserves_hidden_normal_text() {
        let mut screen = Screen::new(80, 24);
        let text = "BEGIN-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-END";
        screen.process(text.as_bytes());
        screen.process(b"\x1b[?1049hALT");
        screen.resize(30, 24);
        screen.resize(80, 24);
        assert!(screen.parser.screen().alternate_screen());
        assert!(screen.parser.screen().contents().contains("ALT"));
        screen.process(b"\x1b[?1049l");
        assert!(screen.parser.screen().contents().contains(text));
        assert_eq!(
            mirror(&screen).screen().contents(),
            screen.parser.screen().contents()
        );
    }
    #[test]
    fn oversized_unfinished_osc_does_not_prevent_subsequent_screen_output() {
        let mut screen = Screen::new(80, 24);
        screen.process(b"\x1b]0;");
        for _ in 0..1024 {
            screen.process(&[b'x'; 8192]);
        }
        screen.process(b"\x07AFTER-OSC");
        assert_eq!(screen.history_bytes(), 0);
        assert!(screen.parser.screen().contents().contains("AFTER-OSC"));
    }
    #[test]
    fn history_never_exceeds_four_mib_and_keeps_newest_rows() {
        let mut screen = Screen::new(500, 24);
        let line = format!("{}\r\n", "界".repeat(240));
        for _ in 0..7000 {
            screen.process(line.as_bytes());
        }
        screen.process(b"LATEST-MARKER\r\n");
        assert!(screen.history_bytes() <= HISTORY_BYTES);
        assert!(screen.truncated());
        let snapshot: Vec<u8> = screen.snapshot().into_iter().flatten().collect();
        assert!(String::from_utf8(snapshot)
            .unwrap()
            .contains("LATEST-MARKER"));
    }
    #[test]
    fn replay_restores_colors_unicode_cursor_and_alternate_screen() {
        let mut screen = Screen::new(80, 24);
        screen.process("normal 你好\x1b[31mRED\x1b[m".as_bytes());
        screen.process(b"\x1b[?1049h\x1b[Htop-running\x1b[?2004h");
        let mut mirror = vt100::Parser::default();
        for part in screen.snapshot() {
            mirror.process(&part);
        }
        assert!(mirror.screen().alternate_screen());
        assert!(mirror.screen().contents().contains("top-running"));
        assert!(mirror.screen().bracketed_paste());
        mirror.process(b"\x1b[?1049l");
        assert!(mirror.screen().contents().contains("normal 你好"));
        assert!(mirror.screen().contents().contains("RED"));
    }
    #[test]
    fn replay_keeps_partial_control_strings_out_of_visible_text_and_consumes_reset() {
        for (prefix, suffix) in [
            (&b"kept\x1b]0;partial"[..], &b"title\x07after"[..]),
            (&b"kept\x1bPignored"[..], &b"payload\x1b\\after"[..]),
            (&b"kept\x1b]0;partial\x1b"[..], &b"\\after"[..]),
            (&b"kept\x1bcafter"[..], &b"tail"[..]),
        ] {
            let mut screen = Screen::new(80, 24);
            screen.process(prefix);
            let mut mirror = vt100::Parser::default();
            for part in screen.snapshot() {
                mirror.process(&part);
            }
            screen.process(suffix);
            mirror.process(suffix);
            assert_eq!(
                mirror.screen().contents(),
                screen.parser.screen().contents()
            );
        }
    }
    #[test]
    fn replay_preserves_a_split_utf8_character_and_partial_escape() {
        let mut screen = Screen::new(80, 24);
        screen.process(&[0xe4]);
        let mut mirror = vt100::Parser::default();
        for part in screen.snapshot() {
            mirror.process(&part);
        }
        mirror.process(&[0xbd, 0xa0]);
        assert_eq!(mirror.screen().contents(), "你");
        screen.process(&[0xbd, 0xa0]);
        screen.process(b"\x1b[3");
        let mut mirror = vt100::Parser::default();
        for part in screen.snapshot() {
            mirror.process(&part);
        }
        mirror.process(b"1mRED");
        assert!(mirror.screen().contents().contains("你RED"));
        assert_eq!(mirror.screen().fgcolor(), vt100::Color::Idx(1));
    }
}
