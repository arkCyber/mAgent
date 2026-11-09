//! Line editing for the REPL prompt.
//!
//! The REPL used to read with `stdin.read_line`, which is fine for
//! piped input but hostile to a person at a keyboard: ↑ and ↓ arrived
//! as the literal bytes `ESC [ A` / `ESC [ B`, which the dispatcher then
//! dutifully submitted to the LLM as a *task*, and a typo could only be
//! fixed with the terminal's own erase key.
//!
//! The module is two pieces on purpose:
//!
//! * [`LineBuffer`] — a pure text buffer with a cursor. Every editing
//!   semantic (insert, backspace, forward-delete, cursor motion, kill)
//!   lives here and is tested directly, with no terminal in sight.
//! * [`read_line`] — the terminal loop. On a TTY it puts the terminal
//!   into non-canonical, no-echo mode and feeds each key to the buffer,
//!   redrawing after every edit. Everywhere else — piped stdin, a test
//!   harness, a non-Unix target — it falls back to the original
//!   line-at-a-time read, so scripted input keeps working unchanged.
//!
//! Raw mode deliberately leaves `ISIG` alone, so Ctrl-C still sends
//! `SIGINT` and kills the REPL the way it always has. Because that signal
//! terminates the process without unwinding, `Drop` never runs on it — so
//! the guard also installs a `SIGINT` handler that puts the terminal back
//! before the signal is allowed to kill the process. Without it, Ctrl-C
//! left the shell with echo and line editing off.
//!
//! The prompt and the redraws go to the terminal stream — stdout when it
//! is a terminal, stderr otherwise — so a redirected stdout stays the
//! transcript rather than a log of every keystroke. See `ui_on_stdout`.

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};

/// A text buffer with a cursor, measured in `char`s rather than bytes
/// so multi-byte input (the REPL's users type Chinese) edits correctly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineBuffer {
    chars: Vec<char>,
    /// Cursor position as a char index in `0..=chars.len()`.
    cursor: usize,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the contents with `s`, leaving the cursor at the end.
    /// Used by history recall, where the natural place to type next is
    /// after the recalled line.
    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn as_string(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of chars before the cursor — i.e. the prefix that any
    /// caller needs to measure to place the cursor on screen.
    pub fn prefix(&self) -> &[char] {
        &self.chars[..self.cursor]
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Erase the char before the cursor. Returns `false` at the start of
    /// the line, where there is nothing to erase.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.chars.remove(self.cursor);
        true
    }

    /// Erase the char under the cursor. Returns `false` at the end.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.chars.remove(self.cursor);
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn move_home(&mut self) -> bool {
        let moved = self.cursor != 0;
        self.cursor = 0;
        moved
    }

    pub fn move_end(&mut self) -> bool {
        let moved = self.cursor != self.chars.len();
        self.cursor = self.chars.len();
        moved
    }

    /// Ctrl-K: drop the cursor-to-end tail. Returns the number of chars
    /// removed so the caller can decide whether a redraw is needed.
    pub fn kill_to_end(&mut self) -> usize {
        let removed = self.chars.len() - self.cursor;
        self.chars.truncate(self.cursor);
        removed
    }

    /// Ctrl-U: drop everything before the cursor, which lands at 0.
    pub fn kill_to_start(&mut self) -> usize {
        let removed = self.cursor;
        self.chars.drain(..self.cursor);
        self.cursor = 0;
        removed
    }

    /// Ctrl-W: drop the word before the cursor. A word ends at the last
    /// non-blank char, so trailing spaces go with it — matching what
    /// readline's `unix-word-rubout` does.
    pub fn kill_word(&mut self) -> usize {
        let end = self.cursor;
        let mut start = self.cursor;
        while start > 0 && self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.chars.drain(start..end);
        self.cursor = start;
        end - start
    }
}

/// Read one line, with editing and history recall when stdin is a
/// terminal.
///
/// * `prompt` — printed verbatim before the line, and reprinted on every
///   redraw.
/// * `char_width` — display width of a char, the same measure the
///   box-drawing helpers use. Passed in rather than reinvented here so
///   the CLI keeps exactly one width table.
/// * `history` — oldest first; ↑ walks towards the front, ↓ back
///   towards the empty draft line.
///
/// The prompt and every redraw go to the stream `ui_on_stdout` names —
/// terminal UI belongs on the terminal, not baked into whichever stream
/// the shell happened to redirect.
///
/// Returns `None` at end-of-input, which the REPL loop reads as "quit".
pub fn read_line<F>(prompt: &str, char_width: F, history: &VecDeque<String>) -> Option<String>
where
    F: Fn(char) -> usize,
{
    #[cfg(unix)]
    if io::stdin().is_terminal() {
        return tty::read_line(prompt, &char_width, history);
    }
    let _ = char_width;
    fallback(prompt)
}

/// Whether terminal UI — the prompt, the redraws, a screen clear — should
/// be drawn on stdout rather than stderr.
///
/// stdout wins whenever it is the terminal, so the common case (both
/// streams a terminal) behaves exactly as it always has. If stdout is
/// redirected but stderr is not (`… > session.log`, stdin still a
/// terminal) the callers fall back to stderr, which keeps two things
/// true at once: the log holds the transcript instead of one line per
/// keystroke, and the prompt is still visible on the terminal. When
/// *neither* is a terminal there is nobody to draw for, and stderr takes
/// it as the diagnostic stream so stdout stays the program's data.
///
/// Writing the prompt to stdout unconditionally is what made
/// `magent run --repl > session.log` log `❯ h`, `❯ he`, `❯ hel`, … — each
/// with the redraw's `\r\x1b[K` in front of it.
pub(crate) fn ui_on_stdout() -> bool {
    io::stdout().is_terminal()
}

/// The original line-at-a-time read. Also the escape hatch when raw mode
/// cannot be enabled — a bad `tcgetattr` must not cost the user their
/// session.
fn fallback(prompt: &str) -> Option<String> {
    if ui_on_stdout() {
        print!("{}", prompt);
        let _ = io::stdout().flush();
    } else {
        eprint!("{}", prompt);
        let _ = io::stderr().flush();
    }

    let mut line = String::new();
    match io::stdin().read_line(&mut line) {
        // `read_line` returns Ok(0) at EOF — signal the caller to stop.
        Ok(0) => None,
        Ok(_) => Some(line.trim().to_string()),
        // A decode error is **not** EOF. `read_line` reads the line's bytes
        // off the stream before trying to validate them, so the offending
        // bytes are already consumed: report and return an empty line
        // (which the loop skips) so one stray non-UTF-8 byte — a pasted
        // binary blob, a mis-encoded pipe — does not silently end the
        // session. Treating the old `Err(_) => None` as EOF did exactly
        // that, discarding the rest of the input with no message.
        Err(e) => {
            eprintln!("\n  ✗ Input error (line skipped): {}", e);
            Some(String::new())
        }
    }
}

#[cfg(unix)]
mod tty {
    use super::LineBuffer;
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// Byte-level keys. Kept as named constants so the dispatch below
    /// reads as a keymap rather than a table of magic numbers.
    const CTRL_A: u8 = 0x01;
    const CTRL_B: u8 = 0x02;
    const CTRL_D: u8 = 0x04;
    const CTRL_E: u8 = 0x05;
    const CTRL_F: u8 = 0x06;
    const CTRL_K: u8 = 0x0B;
    const CTRL_L: u8 = 0x0C;
    const CTRL_U: u8 = 0x15;
    const CTRL_W: u8 = 0x17;
    const BACKSPACE: u8 = 0x7F;
    const ESC: u8 = 0x1B;

    /// The settings to put back when a signal kills us mid-edit, or null
    /// while no guard is alive.
    ///
    /// `Drop` cannot cover this: `SIGINT`'s default disposition ends the
    /// process without unwinding, so the guard is never dropped and the
    /// terminal stays in raw mode. The handler needs the saved settings
    /// without allocating or locking (neither is signal-safe), so they are
    /// published here as a bare pointer. `tcsetattr` is on POSIX's list of
    /// async-signal-safe functions, which is what makes the restore legal.
    /// Signals are process-wide and there is one prompt at a time, so a
    /// single slot is enough.
    static SAVED_TERMIOS: AtomicPtr<libc::termios> = AtomicPtr::new(std::ptr::null_mut());

    /// Put the terminal back, then let the signal kill the process.
    ///
    /// Only async-signal-safe calls appear here — an atomic load, a
    /// `tcsetattr`, and the `sigaction`/`raise` re-raise. No allocation,
    /// no locks, no formatting.
    extern "C" fn restore_on_sigint(sig: libc::c_int) {
        let saved = SAVED_TERMIOS.load(Ordering::Acquire);
        if !saved.is_null() {
            // SAFETY: a non-null value points at the `termios` boxed by a
            // live `RawMode`; `drop` clears the pointer before that box is
            // freed, so the target is valid for the whole time the handler
            // is installed.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, saved);
            }
        }
        // Hand SIGINT back to its default disposition and re-raise: the
        // REPL dies from the signal exactly as it did before the guard
        // existed, so a shell still sees "Interrupt" and the exit status
        // is unchanged.
        // SAFETY: `action` is a zeroed `sigaction` whose handler is
        // `SIG_DFL`; installing it is what `signal(2)` does internally.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &action, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Terminal state guard: whatever flags the terminal had before we
    /// touched them are restored on drop, including on the panic path.
    struct RawMode {
        fd: i32,
        /// Boxed so its address is stable across the move out of
        /// [`RawMode::enable`] — [`SAVED_TERMIOS`] points into it.
        saved: Box<libc::termios>,
        /// SIGINT's disposition before the guard took it over, restored on
        /// drop so a program that installed its own handler keeps it.
        previous: Option<libc::sigaction>,
    }

    impl RawMode {
        fn enable() -> Option<Self> {
            let fd = libc::STDIN_FILENO;
            // SAFETY: `fd` is a valid descriptor (stdin) and `saved` is a
            // live, properly aligned `termios`. `tcgetattr` only writes on
            // success, which the return code checks.
            let mut saved: Box<libc::termios> = Box::new(unsafe { std::mem::zeroed() });
            if unsafe { libc::tcgetattr(fd, saved.as_mut()) } != 0 {
                return None;
            }
            let mut raw = *saved;
            // Canonical mode is what buffers a whole line and does its own
            // echo; both must go for per-keystroke editing. `ISIG` is left
            // set so Ctrl-C still raises SIGINT.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            // Return as soon as one byte is available instead of waiting
            // for the line to end. (VMIN/VTIME only take effect once
            // ICANON is cleared.)
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            // SAFETY: same descriptor, `raw` is a fully initialised copy.
            if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
                return None;
            }
            // Publish the settings before the handler can observe them, so
            // a Ctrl-C landing between these two steps still restores the
            // terminal. A failure to install the handler is not fatal:
            // `Drop` still covers the normal and panic paths, so line
            // editing is worth more than refusing the guard.
            SAVED_TERMIOS.store(saved.as_mut() as *mut libc::termios, Ordering::Release);
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = restore_on_sigint as *const () as libc::sighandler_t;
            action.sa_flags = 0;
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: `action` is a fully initialised `sigaction` with a
            // handler of the right signature; `previous` receives the old
            // one.
            let installed = unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous) } == 0;
            if !installed {
                SAVED_TERMIOS.store(std::ptr::null_mut(), Ordering::Release);
            }
            Some(Self {
                fd,
                saved,
                previous: installed.then_some(previous),
            })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // Retract the handler's save area, then hand SIGINT back to
            // whoever had it. Clearing first means a signal landing in
            // between reads a null pointer instead of memory about to be
            // freed.
            SAVED_TERMIOS.store(std::ptr::null_mut(), Ordering::Release);
            if let Some(previous) = self.previous.take() {
                // SAFETY: `previous` was filled by `sigaction` above.
                unsafe {
                    libc::sigaction(libc::SIGINT, &previous, std::ptr::null_mut());
                }
            }
            // SAFETY: restoring the struct we read a moment ago.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, self.saved.as_ref());
            }
        }
    }

    /// One byte, or `None` on EOF. A read error is treated as EOF: there
    /// is no useful recovery for a keyboard that stopped working.
    fn read_byte<R: Read>(input: &mut R) -> Option<u8> {
        let mut b = [0u8; 1];
        match input.read(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    }

    /// Reassemble a UTF-8 char from `first` plus its continuation bytes.
    /// A truncated sequence (or a stray continuation byte) decodes to
    /// `None`, and the char is dropped rather than inserted as a
    /// replacement glyph — the alternative is putting `U+FFFD` on the
    /// user's command line.
    fn read_utf8<R: Read>(input: &mut R, first: u8) -> Option<char> {
        let extra = match first {
            0x00..=0x7F => 0,
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            // A continuation byte with no lead, or an invalid lead.
            _ => return None,
        };
        let mut buf = vec![first];
        for _ in 0..extra {
            buf.push(read_byte(input)?);
        }
        std::str::from_utf8(&buf)
            .ok()
            .and_then(|s| s.chars().next())
    }

    /// Redraw the line in place.
    ///
    /// `\r` returns to column 0, `ESC[K` wipes to end of line, then the
    /// prompt and text are rewritten and the cursor is walked back to the
    /// right place. This assumes the line fits the terminal width; a line
    /// that wraps will redraw progressively more wrongly. A real editor
    /// tracks row/column across wraps — the REPL's lines are short tasks,
    /// and the fallback is cosmetic, so that machinery is not worth it.
    fn redraw<W: Write, F: Fn(char) -> usize>(
        out: &mut W,
        prompt: &str,
        buf: &LineBuffer,
        char_width: &F,
    ) {
        let text = buf.as_string();
        let prefix_width: usize = buf.prefix().iter().map(|&c| char_width(c)).sum();
        let text_width: usize = text.chars().map(char_width).sum();
        let _ = write!(out, "\r\x1b[K{}{}", prompt, text);
        // Walk the cursor back from the end of the text to where it belongs.
        if text_width > prefix_width {
            let _ = write!(out, "\x1b[{}D", text_width - prefix_width);
        }
        let _ = out.flush();
    }

    /// Consume the rest of an `ESC [ …` sequence and act on it.
    ///
    /// Reads at most one more byte after the final letter than is needed,
    /// and gives up on anything unrecognised. A lone `ESC` (an Alt-key
    /// prefix on most terminals) therefore blocks until the next key —
    /// there is no portable timeout without `VTIME` juggling, and the
    /// cost of getting it wrong is a stalled prompt.
    #[allow(clippy::too_many_arguments)]
    fn handle_escape<R: Read, W: Write, F: Fn(char) -> usize>(
        input: &mut R,
        out: &mut W,
        prompt: &str,
        buf: &mut LineBuffer,
        char_width: &F,
        history: &VecDeque<String>,
        hist_idx: &mut usize,
        draft: &mut String,
    ) {
        match read_byte(input) {
            // CSI: `ESC [ …` (and the `ESC O …` application-key variant).
            Some(b'[') | Some(b'O') => {}
            _ => return,
        }
        /// Discard the remainder of a CSI sequence: parameter/intermediate
        /// bytes (0x20..=0x3f) up to the final byte (0x40..=0x7e).
        fn discard_csi<R: Read>(input: &mut R) {
            loop {
                match read_byte(input) {
                    Some(x) if (0x40..=0x7e).contains(&x) => return,
                    Some(_) => {}
                    None => return,
                }
            }
        }

        match read_byte(input) {
            Some(b'A') => recall(buf, history, hist_idx, draft, -1),
            Some(b'B') => recall(buf, history, hist_idx, draft, 1),
            Some(b'C') => {
                buf.move_right();
            }
            Some(b'D') => {
                buf.move_left();
            }
            Some(b'H') => {
                buf.move_home();
            }
            Some(b'F') => {
                buf.move_end();
            }
            // `ESC [ n ~` — the tilde-terminated family.
            Some(d) if d.is_ascii_digit() => {
                // Consume the (possibly multi-digit) parameter…
                let mut param = (d - b'0') as u32;
                loop {
                    match read_byte(input) {
                        Some(b'~') => break,
                        Some(x) if x.is_ascii_digit() => {
                            // A long digit run (`ESC [ 99999999999 ~`) used to
                            // overflow `u32` and panic in dev builds.
                            param = param
                                .saturating_mul(10)
                                .saturating_add((x - b'0') as u32);
                        }
                        // Modifier sequences (`ESC [ 1 ; 5 C` for Ctrl-Right)
                        // continue with parameter bytes; swallow the rest of
                        // the CSI instead of typing `5C` into the line.
                        Some(x) if (0x20..=0x3f).contains(&x) => {
                            discard_csi(input);
                            return;
                        }
                        _ => return,
                    }
                }
                match param {
                    1 | 7 => {
                        buf.move_home();
                    }
                    3 => {
                        buf.delete_forward();
                    }
                    4 | 8 => {
                        buf.move_end();
                    }
                    _ => {}
                }
            }
            // An unknown final byte (e.g. `ESC [ Z` for Shift-Tab) is a
            // complete sequence: ignore it. Parameter/intermediate bytes
            // (`ESC [ ; 5 C`, focus events) need the rest consumed through
            // their final byte and must never be typed as text.
            Some(x) if (0x40..=0x7e).contains(&x) => {}
            Some(x) if (0x20..=0x3f).contains(&x) => discard_csi(input),
            _ => return,
        }
        redraw(out, prompt, buf, char_width);
    }

    /// Walk the history list. `step` is -1 for ↑ (older) and +1 for ↓
    /// (newer). The line being edited when the walk starts is stashed in
    /// `draft` and restored on the way back past the end, so a half-typed
    /// command is never lost to an accidental ↑.
    fn recall(
        buf: &mut LineBuffer,
        history: &VecDeque<String>,
        hist_idx: &mut usize,
        draft: &mut String,
        step: isize,
    ) {
        // One past the end means "the new line being typed".
        if *hist_idx == history.len() {
            if step < 0 {
                *draft = buf.as_string();
            } else {
                return;
            }
        }
        let next = *hist_idx as isize + step;
        if next < 0 || next > history.len() as isize {
            return;
        }
        *hist_idx = next as usize;
        if *hist_idx == history.len() {
            buf.set(draft);
        } else {
            buf.set(&history[*hist_idx]);
        }
    }

    pub(super) fn read_line<F>(
        prompt: &str,
        char_width: &F,
        history: &VecDeque<String>,
    ) -> Option<String>
    where
        F: Fn(char) -> usize,
    {
        let Some(_guard) = RawMode::enable() else {
            // Could not enter raw mode; the plain read still works.
            return super::fallback(prompt);
        };
        let mut input = io::stdin();
        if super::ui_on_stdout() {
            edit(prompt, char_width, history, &mut input, &mut io::stdout())
        } else {
            edit(prompt, char_width, history, &mut input, &mut io::stderr())
        }
    }

    /// The key loop, on any byte source and sink.
    ///
    /// Split out from [`read_line`] so the whole keymap — arrows, history
    /// recall, Ctrl-D, multi-byte input — is exercised by unit tests over
    /// a `Cursor<Vec<u8>>`. Raw mode is the *caller's* problem; there is
    /// nothing terminal-specific below.
    pub(super) fn edit<R: Read, W: Write, F>(
        prompt: &str,
        char_width: &F,
        history: &VecDeque<String>,
        input: &mut R,
        out: &mut W,
    ) -> Option<String>
    where
        F: Fn(char) -> usize,
    {
        let mut buf = LineBuffer::new();
        let mut hist_idx = history.len();
        let mut draft = String::new();

        let _ = write!(out, "{}", prompt);
        let _ = out.flush();

        loop {
            let byte = match read_byte(input) {
                Some(b) => b,
                None => {
                    // EOF. A partial line is still input: a pipe that ends
                    // without a trailing newline (`printf 'help' | magent
                    // run --repl`) must run its last command, not drop it.
                    // Only an empty buffer means "no more input" — that is
                    // the bare Ctrl-D case, and the next `read_line` call
                    // then sees EOF again and ends the session.
                    if buf.is_empty() {
                        let _ = write!(out, "\r\n");
                        let _ = out.flush();
                        return None;
                    }
                    return Some(buf.as_string().trim().to_string());
                }
            };
            match byte {
                b'\r' | b'\n' => {
                    let _ = write!(out, "\r\n");
                    let _ = out.flush();
                    return Some(buf.as_string().trim().to_string());
                }
                CTRL_D => {
                    // Mid-line Ctrl-D deletes forward, as readline does;
                    // on an empty line it means end-of-input.
                    if buf.is_empty() {
                        let _ = write!(out, "\r\n");
                        let _ = out.flush();
                        return None;
                    }
                    buf.delete_forward();
                }
                BACKSPACE | 0x08 => {
                    buf.backspace();
                }
                CTRL_A => {
                    buf.move_home();
                }
                CTRL_E => {
                    buf.move_end();
                }
                CTRL_B => {
                    buf.move_left();
                }
                CTRL_F => {
                    buf.move_right();
                }
                CTRL_K => {
                    buf.kill_to_end();
                }
                CTRL_U => {
                    buf.kill_to_start();
                }
                CTRL_W => {
                    buf.kill_word();
                }
                CTRL_L => {
                    // Clear the screen, then put the line back at the top.
                    let _ = write!(out, "\x1b[2J\x1b[H");
                }
                ESC => {
                    handle_escape(
                        input,
                        out,
                        prompt,
                        &mut buf,
                        char_width,
                        history,
                        &mut hist_idx,
                        &mut draft,
                    );
                    continue;
                }
                // Tabs and the remaining control bytes have no meaning
                // here; dropping them beats inserting invisibly.
                b if b < 0x20 => continue,
                _ => {
                    // ASCII arrives whole; anything ≥ 0x80 is the lead
                    // byte of a multi-byte char.
                    let c = if byte < 0x80 {
                        Some(byte as char)
                    } else {
                        read_utf8(input, byte)
                    };
                    match c {
                        Some(c) => buf.insert(c),
                        None => continue,
                    }
                }
            }
            redraw(out, prompt, &buf, char_width);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Feed `keys` to the editor and return the line plus everything
        /// it wrote. `char_width` is 1 per char: screen placement is not
        /// what these tests check, the *keymap* is.
        fn feed(keys: &[u8], history: &[&str]) -> (Option<String>, String) {
            let hist: VecDeque<String> = history.iter().map(|s| s.to_string()).collect();
            let mut input = std::io::Cursor::new(keys.to_vec());
            let mut out: Vec<u8> = Vec::new();
            let line = edit("> ", &|_| 1, &hist, &mut input, &mut out);
            (line, String::from_utf8_lossy(&out).into_owned())
        }

        #[test]
        fn typing_then_enter_returns_the_line() {
            let (line, out) = feed(b"hello\r", &[]);
            assert_eq!(line.as_deref(), Some("hello"));
            assert!(
                out.starts_with("> "),
                "the prompt is written first: {out:?}"
            );
        }

        #[test]
        fn a_newline_submits_like_a_carriage_return() {
            let (line, _) = feed(b"hi\n", &[]);
            assert_eq!(line.as_deref(), Some("hi"));
        }

        #[test]
        fn backspace_erases_the_previous_char() {
            let (line, _) = feed(b"helo\x7flo\r", &[]);
            assert_eq!(line.as_deref(), Some("hello"));
        }

        #[test]
        fn left_arrow_moves_the_cursor_so_the_next_char_lands_before_it() {
            // "ac", cursor left, "b" → "abc".
            let (line, _) = feed(b"ac\x1b[Db\r", &[]);
            assert_eq!(line.as_deref(), Some("abc"));
        }

        #[test]
        fn right_arrow_moves_the_cursor_forwards_off_home() {
            // "bc", home, right past 'b', insert 'a' → "bac".
            let (line, _) = feed(b"bc\x1b[H\x1b[Ca\r", &[]);
            assert_eq!(line.as_deref(), Some("bac"));
        }

        #[test]
        fn home_and_end_keys_bound_the_cursor() {
            let (line, _) = feed(b"bc\x1b[H\x1b[Da\x1b[F!\r", &[]);
            assert_eq!(line.as_deref(), Some("abc!"));
        }

        #[test]
        fn the_delete_key_removes_the_char_under_the_cursor() {
            // "abc", home, Delete → "bc".
            let (line, _) = feed(b"abc\x1b[H\x1b[3~\r", &[]);
            assert_eq!(line.as_deref(), Some("bc"));
        }

        #[test]
        fn ctrl_d_on_an_empty_line_is_end_of_input() {
            let (line, _) = feed(&[CTRL_D], &[]);
            assert_eq!(line, None);
        }

        #[test]
        fn ctrl_d_mid_line_deletes_forward_instead_of_quitting() {
            let (line, _) = feed(b"abc\x1b[D\x04\r", &[]);
            assert_eq!(line.as_deref(), Some("ab"));
        }

        #[test]
        fn ctrl_a_ctrl_e_bracket_the_cursor() {
            let (line, _) = feed(b"bc\x01a\x05!\r", &[]);
            assert_eq!(line.as_deref(), Some("abc!"));
        }

        #[test]
        fn ctrl_w_deletes_the_previous_word() {
            // The leftover trailing blank is trimmed when the line is
            // submitted, so the result is "run the".
            let (line, _) = feed(b"run the task\x17\r", &[]);
            assert_eq!(line.as_deref(), Some("run the"));
        }

        #[test]
        fn ctrl_u_and_ctrl_k_clear_the_two_halves() {
            let (line, _) = feed(b"hello world\x0b\x15\r", &[]);
            assert_eq!(line.as_deref(), Some(""));
        }

        #[test]
        fn multi_byte_input_arrives_as_whole_chars() {
            // "你好" in UTF-8, then backspace → "你".
            let (line, _) = feed("你好\x7f\r".as_bytes(), &[]);
            assert_eq!(line.as_deref(), Some("你"));
        }

        #[test]
        fn up_recalls_the_newest_task() {
            let (line, _) = feed(b"\x1b[A\r", &["first", "second"]);
            assert_eq!(line.as_deref(), Some("second"));
        }

        #[test]
        fn down_restores_the_half_typed_draft() {
            let (line, _) = feed(b"draft\x1b[A\x1b[B\r", &["first", "second"]);
            assert_eq!(
                line.as_deref(),
                Some("draft"),
                "the half-typed line must survive the round trip"
            );
        }

        #[test]
        fn repeated_up_walks_back_through_history() {
            let (line, _) = feed(b"\x1b[A\x1b[A\r", &["first", "second"]);
            assert_eq!(line.as_deref(), Some("first"));
        }

        #[test]
        fn up_then_down_past_the_end_lands_back_on_the_draft() {
            // ↑ to "second", ↑ to "first", ↓ to "second", ↓ to the draft.
            let (line, _) = feed(b"x\x1b[A\x1b[A\x1b[B\x1b[B\r", &["first", "second"]);
            assert_eq!(line.as_deref(), Some("x"));
        }

        #[test]
        fn down_on_a_line_that_was_never_recalled_does_nothing() {
            let (line, _) = feed(b"abc\x1b[B\r", &["first"]);
            assert_eq!(line.as_deref(), Some("abc"));
        }

        #[test]
        fn recalled_text_is_editable_not_frozen() {
            // ↑ to "second", then append "!".
            let (line, _) = feed(b"\x1b[A!\r", &["first", "second"]);
            assert_eq!(line.as_deref(), Some("second!"));
        }

        #[test]
        fn a_pipe_ending_without_a_newline_still_runs_its_last_command() {
            // `printf 'help' | magent run --repl` must not lose the `help`.
            let hist = VecDeque::new();
            let mut input = std::io::Cursor::new(b"abc".to_vec());
            let mut out: Vec<u8> = Vec::new();
            let first = edit("> ", &|_| 1, &hist, &mut input, &mut out);
            assert_eq!(first.as_deref(), Some("abc"));
            // …and the next read is the one that reports end-of-input.
            let second = edit("> ", &|_| 1, &hist, &mut input, &mut out);
            assert_eq!(second, None);
        }

        #[test]
        fn eof_on_an_empty_buffer_is_end_of_input() {
            let mut input = std::io::Cursor::new(Vec::new());
            let mut out: Vec<u8> = Vec::new();
            assert_eq!(
                edit("> ", &|_| 1, &VecDeque::new(), &mut input, &mut out),
                None
            );
        }

        #[test]
        fn a_stray_continuation_byte_is_dropped_not_garbled() {
            // 0x80 with no lead byte, then a normal char.
            let (line, _) = feed(b"\x80ok\r", &[]);
            assert_eq!(line.as_deref(), Some("ok"));
        }

        #[test]
        fn modifier_csi_sequences_are_swallowed_not_typed() {
            // Ctrl-Right / Ctrl-Left: `ESC [ 1 ; 5 C/D`. The `;5C` tail used
            // to be inserted into the line as literal text.
            let (line, _) = feed(b"ab\x1b[1;5Cc\r", &[]);
            assert_eq!(line.as_deref(), Some("abc"));
            let (line, _) = feed(b"ab\x1b[1;5Dc\r", &[]);
            assert_eq!(line.as_deref(), Some("abc"));
        }

        #[test]
        fn a_huge_csi_parameter_does_not_overflow() {
            // The `ESC [ n ~` parser multiplied without a bound; a long digit
            // run overflowed u32 and panicked in debug builds.
            let (line, _) = feed(b"ab\x1b[99999999999~c\r", &[]);
            assert_eq!(line.as_deref(), Some("abc"));
        }

        #[test]
        fn an_unrecognised_escape_swallows_its_pair_rather_than_type_it() {
            // `ESC x` is an Alt-key chord, not text: the pair is consumed
            // and the following `b` is typed normally. The point is that
            // `x` does not end up on the command line as if it were typed
            // by itself.
            let (line, _) = feed(b"a\x1bxb\r", &[]);
            assert_eq!(line.as_deref(), Some("ab"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> LineBuffer {
        let mut b = LineBuffer::new();
        b.set(s);
        b
    }

    #[test]
    fn set_puts_the_cursor_at_the_end() {
        let b = buf("hello");
        assert_eq!(b.as_string(), "hello");
        assert_eq!(b.cursor(), 5);
    }

    #[test]
    fn insert_types_at_the_cursor_not_at_the_end() {
        let mut b = buf("ac");
        b.move_left();
        b.insert('b');
        assert_eq!(b.as_string(), "abc");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn backspace_removes_the_char_before_the_cursor() {
        let mut b = buf("abc");
        b.move_left();
        assert!(b.backspace());
        assert_eq!(b.as_string(), "ac");
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn backspace_at_the_start_reports_no_change() {
        let mut b = buf("abc");
        b.move_home();
        assert!(!b.backspace());
        assert_eq!(b.as_string(), "abc");
    }

    #[test]
    fn delete_forward_removes_the_char_under_the_cursor() {
        let mut b = buf("abc");
        b.move_home();
        assert!(b.delete_forward());
        assert_eq!(b.as_string(), "bc");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn delete_forward_at_the_end_reports_no_change() {
        let mut b = buf("abc");
        assert!(!b.delete_forward());
        assert_eq!(b.as_string(), "abc");
    }

    #[test]
    fn cursor_motion_stops_at_both_ends() {
        let mut b = buf("ab");
        assert!(!b.move_right(), "right at the end must not wrap");
        assert_eq!(b.cursor(), 2);
        b.move_home();
        assert_eq!(b.cursor(), 0);
        assert!(!b.move_left(), "left at the start must not wrap");
        assert!(!b.move_home(), "already home");
        assert!(b.move_end());
        assert!(!b.move_end(), "already at the end");
    }

    #[test]
    fn kill_to_end_drops_only_the_tail() {
        let mut b = buf("hello world");
        for _ in 0..6 {
            b.move_left();
        }
        assert_eq!(b.kill_to_end(), 6, "the six chars after the cursor");
        assert_eq!(b.as_string(), "hello");
        assert_eq!(b.cursor(), 5);
        assert_eq!(b.kill_to_end(), 0, "nothing left to kill");
    }

    #[test]
    fn kill_to_start_drops_only_the_head() {
        let mut b = buf("hello world");
        for _ in 0..6 {
            b.move_left();
        }
        assert_eq!(b.kill_to_start(), 5);
        assert_eq!(b.as_string(), " world");
        assert_eq!(b.cursor(), 0);
        assert_eq!(b.kill_to_start(), 0);
    }

    #[test]
    fn kill_word_takes_the_previous_word_and_leaves_the_space() {
        let mut b = buf("run the task");
        assert_eq!(b.kill_word(), 4);
        assert_eq!(b.as_string(), "run the ");
        assert_eq!(b.cursor(), 8);
    }

    #[test]
    fn kill_word_from_a_trailing_blank_eats_the_blank_too() {
        let mut b = buf("two words  ");
        assert_eq!(b.kill_word(), 7, "the two trailing blanks plus 'words'");
        assert_eq!(b.as_string(), "two ");
    }

    #[test]
    fn kill_word_at_the_start_is_a_no_op() {
        let mut b = buf("word");
        b.move_home();
        assert_eq!(b.kill_word(), 0);
        assert_eq!(b.as_string(), "word");
    }

    #[test]
    fn multi_byte_chars_count_as_one_step() {
        let mut b = buf("你好");
        assert_eq!(b.cursor(), 2, "two chars, not six bytes");
        assert!(b.backspace());
        assert_eq!(b.as_string(), "你");
        b.insert('好');
        assert_eq!(b.as_string(), "你好");
    }

    #[test]
    fn set_from_history_replaces_and_parks_the_cursor_at_the_end() {
        let mut b = buf("half-typed");
        b.move_home();
        b.set("recalled entry");
        assert_eq!(b.as_string(), "recalled entry");
        assert_eq!(b.cursor(), 14);
    }
}
