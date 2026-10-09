//! viewer — a universal read-only file viewer for the Fe2O3 suite.
//!
//! Given a file it renders a type-appropriate preview (spreadsheet table,
//! text/markdown, word doc via pandoc, slides, pdf, image) and launches the
//! right editor on `e`. With no file (or via `o`) it opens `pointer` as the
//! file browser (--pick mode, like kastrup's attach). Formats are a data table
//! (see `registry`) so new ones are a config line, not code. Input is blocking
//! — zero idle cost.

mod registry;
mod render;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crust::{style, Crust, Input, Pane};
use registry::{Handler, Kind};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Keybindings shown by the `?` popup.
const KEYS: &[(&str, &str)] = &[
    ("j k  \u{2191}\u{2193}", "scroll"),
    ("g  G", "top / bottom"),
    ("PgUp PgDn", "page up / down"),
    ("/  n N", "find a word / next, previous"),
    ("e  Enter", "edit \u{2014} right editor (slide decks \u{2192} Claude)"),
    ("Ctrl+a", "edit with Claude — full session (uses the file-type skill)"),
    ("x", "open externally (xdg-open)"),
    ("o", "browse files (pointer)"),
    ("?", "show this help"),
    ("q", "quit"),
];

struct App {
    file: Option<(PathBuf, Handler)>,
    img: Option<glow::Display>,
    img_shown: bool,
    term_w: u16,
    term_h: u16,
    top: Pane,
    body: Pane,
    foot: Pane,
    status: String,
    /// The file as text, as `render` gave it. Empty for a picture.
    text: String,
    /// The word being looked for, in lower case. Empty: none.
    word: String,
    /// The rows of the body that have the word, and which one we are at.
    hits: Vec<usize>,
    hit: usize,
}

impl App {
    fn new() -> Self {
        let (term_w, term_h) = Crust::terminal_size();
        let (top, body, foot) = make_panes(term_w, term_h);
        App {
            file: None, img: None, img_shown: false, term_w, term_h, top, body, foot,
            status: String::new(), text: String::new(), word: String::new(), hits: Vec::new(), hit: 0,
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        let handler = registry::Registry::load().lookup(&path);
        self.file = Some((path, handler));
        self.word.clear();
        self.load_view();
    }

    /// (Re)render the current file. Panes are invalidated first so a full
    /// repaint follows a screen wipe (resize, returning from pointer/an editor).
    fn load_view(&mut self) {
        self.clear_image();
        self.invalidate();
        let Some((path, handler)) = self.file.clone() else { return };
        let wrap = matches!(handler.kind, Kind::Text | Kind::Doc | Kind::Slides);
        self.body.wrap = wrap;
        self.body.word_wrap = wrap;
        if handler.kind == Kind::Image {
            self.text.clear();
            self.show_image(&path);
        } else {
            self.text = render::render(&path, handler.kind);
            self.say_text(0);
        }
        self.draw_chrome();
    }

    /// Put the text in the body with the word marked, scrolled to `ix`,
    /// and note the rows that have the word.
    fn say_text(&mut self, ix: usize) {
        if self.word.is_empty() {
            self.body.set_text(&self.text);
            self.hits.clear();
        } else {
            let word: Vec<char> = self.word.chars().collect();
            let marked: Vec<String> = self.text.lines().map(|l| mark(l, &word)).collect();
            self.body.set_text(&marked.join("\n"));
            self.hits = self.body.visual_lines().iter().enumerate()
                .filter(|(_, row)| crust::strip_ansi(row).to_lowercase().contains(&self.word))
                .map(|(i, _)| i)
                .collect();
        }
        self.body.ix = ix;
        self.body.refresh();
    }

    /// `/`: ask for a word, mark it and go to the first row that has it at
    /// or below the top of the screen. An empty answer takes the marks off.
    fn find(&mut self) {
        if self.text.is_empty() {
            self.status = "a picture has no words to find".into();
            return self.draw_chrome();
        }
        let asked = self.foot.ask_or_cancel("/", "");
        self.foot.invalidate();
        let Some(word) = asked else { return self.draw_chrome() };
        self.word = word.trim().to_lowercase();
        let top = self.body.ix;
        self.say_text(top);
        if self.word.is_empty() {
            return self.draw_chrome();
        }
        // The one before the first hit in view, so that step(1) lands on it.
        let first = self.hits.iter().position(|&row| row >= top).unwrap_or(0);
        self.hit = (first + self.hits.len().max(1) - 1) % self.hits.len().max(1);
        self.step(1);
    }

    /// `n` and `N`: to the next or the previous row with the word, around
    /// the ends.
    fn step(&mut self, by: isize) {
        if self.word.is_empty() {
            self.status = "nothing to find yet: / asks for a word".into();
        } else if self.hits.is_empty() {
            self.status = format!("not in this file: {}", self.word);
        } else {
            let n = self.hits.len() as isize;
            self.hit = (self.hit as isize + by).rem_euclid(n) as usize;
            let last = self.body.visual_line_count().saturating_sub(self.body.h as usize);
            self.body.ix = self.hits[self.hit].saturating_sub(2).min(last);
            self.body.refresh();
            self.status = format!("{}: row {} of {} with it   n next  N previous", self.word, self.hit + 1, n);
        }
        self.draw_chrome();
    }

    fn show_image(&mut self, path: &Path) {
        self.body.say("");
        self.body.full_refresh();
        if self.img.is_none() {
            self.img = Some(glow::Display::new());
        }
        let (x, y, w, h) = (self.body.x, self.body.y, self.body.w, self.body.h);
        let disp = self.img.as_mut().unwrap();
        if !disp.supported() {
            self.body.say("  (no kitty/sixel image protocol in this terminal)");
            return;
        }
        if disp.show(&path.to_string_lossy(), x, y, w, h) {
            self.img_shown = true;
        } else {
            self.body.say("  (could not render image)");
        }
    }

    fn clear_image(&mut self) {
        if self.img_shown {
            if let Some(disp) = self.img.as_mut() {
                disp.clear(self.body.x, self.body.y, self.body.w, self.body.h, self.term_w, self.term_h);
            }
            self.img_shown = false;
        }
    }

    fn draw_chrome(&mut self) {
        let Some((path, handler)) = &self.file else { return };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let top = format!(
            " {}   {}",
            style::coded(name, ",,b"),
            style::coded(handler.kind.label(), "11,,b"),
        );
        self.top.say(&top);
        let edit_hint = if handler.edit.is_some() { "e edit" } else { "e open" };
        let foot = if self.status.is_empty() {
            format!(" j/k scroll  / find  {}  C-a Claude  x open  o browse  ? keys  q quit   viewer {}", edit_hint, VERSION)
        } else {
            format!(" {}", self.status)
        };
        self.foot.say(&foot);
    }

    fn invalidate(&mut self) {
        self.top.invalidate();
        self.body.invalidate();
        self.foot.invalidate();
    }

    fn resize(&mut self) {
        let (w, h) = Crust::terminal_size();
        if w == self.term_w && h == self.term_h {
            return;
        }
        self.clear_image();
        self.term_w = w;
        self.term_h = h;
        let (top, body, foot) = make_panes(w, h);
        self.top = top;
        self.body = body;
        self.foot = foot;
        Crust::clear_screen();
        self.load_view();
    }

    /// Launch `pointer --pick` as the file browser (like kastrup's attach).
    /// Returns the first tagged path, or None if the user quit without one.
    fn pick_file(&mut self, start: Option<&Path>) -> Option<PathBuf> {
        let pick = format!("/tmp/viewer_pick_{}.txt", std::process::id());
        let _ = std::fs::remove_file(&pick);
        self.clear_image();
        Crust::cleanup();
        Crust::clear_screen();
        let _ = std::io::stdout().flush();
        let mut cmd = Command::new("pointer");
        cmd.arg(format!("--pick={}", pick));
        if let Some(s) = start {
            cmd.arg(s);
        }
        let _ = cmd.status();
        Crust::init();
        Crust::clear_screen();
        let chosen = std::fs::read_to_string(&pick)
            .ok()
            .and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()).map(PathBuf::from));
        let _ = std::fs::remove_file(&pick);
        chosen
    }

    /// Open the current file with the system default app (xdg-open), detached
    /// so the GUI app runs alongside the TUI.
    fn xdg_open(&mut self) {
        let Some((path, _)) = &self.file else { return };
        let r = Command::new("xdg-open")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        self.status = match r {
            Ok(_) => "opened externally (xdg-open)".into(),
            Err(e) => format!("xdg-open failed: {}", e),
        };
        self.draw_chrome();
    }

    /// Centred key-help popup (any key closes it), mirroring the other Fe2O3 TUIs.
    fn show_help(&mut self) {
        let (cols, rows) = (self.term_w, self.term_h);
        let kw = KEYS.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(8);
        let cw = KEYS.iter().map(|(_, d)| kw + 2 + d.chars().count()).max().unwrap_or(30);
        let pw = ((cw as u16) + 4).min(cols.saturating_sub(2)).max(24);
        let ph = ((KEYS.len() as u16) + 5).min(rows.saturating_sub(2)).max(6);
        let px = cols.saturating_sub(pw) / 2 + 1;
        let py = rows.saturating_sub(ph) / 2 + 1;
        let mut pane = Pane::new(px, py, pw, ph, 7, 0);
        pane.scroll = false;
        pane.wrap = false;
        pane.border = true;
        pane.border_fg = Some(11);
        let mut s = style::coded(" viewer \u{2014} keys", "11,,b");
        s.push_str("\n\n");
        for (k, d) in KEYS {
            s.push_str(&format!(" {}  {}\n", style::coded(&format!("{:>kw$}", k), "14,,b"), d));
        }
        s.push_str("\n Press any key to close.");
        pane.say(&s);
        pane.border_refresh();
        let _ = Input::getchr(None);
        self.load_view(); // repaint over the popup (re-shows an image if any)
    }

    fn launch_edit(&mut self) {
        let Some((path, handler)) = self.file.clone() else { return };
        // Slide decks have no sensible text editor — `e`/Enter opens the
        // Claude session (with the live PDF preview) instead.
        if matches!(handler.kind, Kind::Slides) {
            return self.ai_edit();
        }
        let argv = match self.edit_argv() {
            Some(a) => a,
            None => {
                self.status = "no editor configured for this type".into();
                self.draw_chrome();
                return;
            }
        };
        self.clear_image();
        Crust::cleanup();
        let _ = Command::new(&argv[0]).args(&argv[1..]).arg(&path).status();
        Crust::init();
        Crust::clear_screen();
        self.status.clear();
        self.load_view();
    }

    /// Edit with an integrated Claude session. Seeds `claude` with the file path
    /// and the user's instruction; the matching Claude Code skill (pptx / docx /
    /// xlsx / pdf …) activates by file type and edits in place, preserving layout.
    fn ai_edit(&mut self) {
        let Some((path, handler)) = self.file.clone() else { return };
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        let instr = match self.foot.ask_or_cancel("Claude edit \u{2014} what to change: ", "") {
            Some(s) => s,
            None => return, // cancelled
        };
        let mut prompt = if instr.trim().is_empty() {
            format!(
                "Edit the file {}, preserving its existing layout and formatting. Ask me what changes I want.",
                abs.display()
            )
        } else {
            format!(
                "Edit the file {} \u{2014} {}. Preserve its existing layout and formatting as much as possible.",
                abs.display(),
                instr.trim()
            )
        };
        // Slide decks: keep a live PDF preview in sync. Tell Claude to
        // re-render after every change; `slidepreview` rebuilds the PDF and
        // zathura (opened once on another workspace) auto-reloads it.
        if matches!(handler.kind, Kind::Slides) {
            prompt.push_str(&format!(
                " This is a slide deck. After each change you make to it, run `slidepreview {}` in the shell so my live PDF preview refreshes. Run it once now to show the current state.",
                abs.display()
            ));
        }
        self.clear_image();
        Crust::cleanup();
        let mut cmd = Command::new("claude");
        cmd.arg(prompt);
        if let Some(dir) = abs.parent() {
            cmd.current_dir(dir);
        }
        let _ = cmd.status();
        Crust::init();
        Crust::clear_screen();
        self.status.clear();
        self.load_view();
    }

    fn edit_argv(&self) -> Option<Vec<String>> {
        let handler = &self.file.as_ref()?.1;
        let raw = match &handler.edit {
            Some(cmd) => cmd.clone(),
            None => "xdg-open".to_string(), // defer to the system default
        };
        let mut parts: Vec<String> = raw.split_whitespace().map(String::from).collect();
        if parts.is_empty() {
            return None;
        }
        if let Some(var) = parts[0].strip_prefix('$') {
            parts[0] = std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "vim".into());
        }
        Some(parts)
    }

    /// Returns false to quit.
    fn handle(&mut self, key: &str) -> bool {
        if !self.status.is_empty() {
            self.status.clear();
            self.draw_chrome();
        }
        match key {
            "q" | "ESC" => return false,
            "j" | "DOWN" => self.body.linedown(),
            "k" | "UP" => self.body.lineup(),
            "PgDOWN" | " " => self.body.pagedown(),
            "PgUP" | "b" => self.body.pageup(),
            "g" | "HOME" => self.body.top(),
            "G" | "END" => self.body.bottom(),
            "/" => self.find(),
            "n" => self.step(1),
            "N" => self.step(-1),
            "e" | "ENTER" => self.launch_edit(),
            "C-A" => self.ai_edit(), // Fe2O3-standard: full CC session (interactive)
            "x" => self.xdg_open(),
            "?" => self.show_help(),
            "o" => {
                let start = self.file.as_ref().and_then(|(p, _)| p.parent().map(Path::to_path_buf));
                if let Some(p) = self.pick_file(start.as_deref()) {
                    self.open_file(p);
                } else {
                    self.load_view(); // cancelled — repaint current file
                }
            }
            "RESIZE" => self.resize(),
            _ => {}
        }
        true
    }
}

/// `line` with every `word` in it shown in reverse. `word` is in lower
/// case. The colour codes of the line stay where they are, and a match
/// keeps its mark across them.
fn mark(line: &str, word: &[char]) -> String {
    let bytes = line.as_bytes();
    // Each character on screen: where it starts and ends in `line`.
    let mut shown: Vec<(usize, usize, char)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i = after_escape(bytes, i);
            continue;
        }
        let c = line[i..].chars().next().unwrap_or(' ');
        shown.push((i, i + c.len_utf8(), c.to_lowercase().next().unwrap_or(c)));
        i += c.len_utf8();
    }
    let mut out = String::with_capacity(line.len() + 16);
    let (mut copied, mut k) = (0, 0);
    while !word.is_empty() && k + word.len() <= shown.len() {
        let here = &shown[k..k + word.len()];
        if !here.iter().map(|s| s.2).eq(word.iter().copied()) {
            k += 1;
            continue;
        }
        out.push_str(&line[copied..here[0].0]);
        copied = here[0].0;
        for s in here {
            // A colour code in the middle of a match may switch the mark
            // off, so it is switched on again after each one.
            out.push_str(&line[copied..s.0]);
            out.push_str(style::REVERSE);
            out.push_str(&line[s.0..s.1]);
            copied = s.1;
        }
        out.push_str(style::REVERSE_OFF);
        k += word.len();
    }
    out.push_str(&line[copied..]);
    out
}

/// The index just past the escape sequence that starts at `i`.
fn after_escape(b: &[u8], i: usize) -> usize {
    match b.get(i + 1) {
        // Colours and the like: up to the letter that ends them.
        Some(b'[') => (i + 2..b.len()).find(|&j| (0x40..=0x7e).contains(&b[j])).map_or(b.len(), |j| j + 1),
        // Links and titles: up to a bell, or ESC and a backslash.
        Some(b']') => (i + 2..b.len())
            .find(|&j| b[j] == 0x07 || (b[j] == 0x1b && b.get(j + 1) == Some(&b'\\')))
            .map_or(b.len(), |j| if b[j] == 0x07 { j + 1 } else { j + 2 }),
        Some(_) => i + 2,
        None => b.len(),
    }
}

fn make_panes(w: u16, h: u16) -> (Pane, Pane, Pane) {
    let mut top = Pane::new(1, 1, w, 1, 15, 240); // white on mid-grey
    let body = Pane::new(1, 2, w, h.saturating_sub(2), 7, 0);
    let mut foot = Pane::new(1, h, w, 1, 0, 6);
    top.wrap = false;
    foot.wrap = false;
    (top, body, foot)
}

fn main() {
    let arg = std::env::args().nth(1);
    if let Some(ref p) = arg {
        if !Path::new(p).exists() {
            eprintln!("viewer: no such file: {}", p);
            std::process::exit(1);
        }
    }

    Crust::init();
    Crust::set_title("viewer");
    Crust::clear_screen();
    let mut app = App::new();
    let initial = match arg {
        Some(p) => Some(PathBuf::from(p)),
        None => app.pick_file(None), // no file → browse with pointer
    };
    match initial {
        Some(p) => app.open_file(p),
        None => {
            app.clear_image();
            Crust::cleanup();
            return;
        }
    }
    loop {
        match Input::getchr(None) {
            Some(k) => {
                if !app.handle(&k) {
                    break;
                }
            }
            None => break,
        }
    }
    app.clear_image();
    Crust::cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(w: &str) -> Vec<char> { w.chars().collect() }

    #[test]
    fn a_word_is_marked_wherever_it_stands_whatever_its_case() {
        let (on, off) = (style::REVERSE, style::REVERSE_OFF);
        assert_eq!(mark("The cat and the Cat", &word("cat")),
                   format!("The {on}c{on}a{on}t{off} and the {on}C{on}a{on}t{off}"));
        assert_eq!(mark("no such thing", &word("cat")), "no such thing");
        assert_eq!(mark("blåbær og BLÅBÆR", &word("blåbær")).matches(off).count(), 2);
        assert_eq!(mark("anything", &[]), "anything");
    }

    #[test]
    fn a_match_keeps_its_mark_across_colour_codes() {
        // "foobar" in two colours, as a syntax highlighter writes it.
        let line = format!("{}{}", style::fg("foo", 81), style::fg("bar", 220));
        let got = mark(&line, &word("oba"));
        assert_eq!(crust::strip_ansi(&got), "foobar", "the text is as it was");
        // Every letter of the match has the mark switched on just before it.
        for letter in ["o", "b", "a"] {
            assert!(got.contains(&format!("{}{}", style::REVERSE, letter)), "{letter} in {got:?}");
        }
        assert_eq!(got.matches(style::REVERSE_OFF).count(), 1);
        // A link around the word is skipped as one piece.
        let link = style::hyperlink("https://example.org/cat", "a cat here");
        assert_eq!(mark(&link, &word("cat")).matches(style::REVERSE_OFF).count(), 1);
    }
}
