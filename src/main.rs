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
    ("e  Enter", "edit \u{2014} launch the right editor"),
    ("c", "edit with Claude (uses the file-type skill)"),
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
}

impl App {
    fn new() -> Self {
        let (term_w, term_h) = Crust::terminal_size();
        let (top, body, foot) = make_panes(term_w, term_h);
        App { file: None, img: None, img_shown: false, term_w, term_h, top, body, foot, status: String::new() }
    }

    fn open_file(&mut self, path: PathBuf) {
        let handler = registry::Registry::load().lookup(&path);
        self.file = Some((path, handler));
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
            self.show_image(&path);
        } else {
            let content = render::render(&path, handler.kind);
            self.body.ix = 0;
            self.body.say(&content);
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
            format!(" j/k scroll  {}  c Claude  x open  o browse  ? keys  q quit   viewer {}", edit_hint, VERSION)
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
        let Some((path, _)) = self.file.clone() else { return };
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
            "e" | "ENTER" => self.launch_edit(),
            "c" => self.ai_edit(),
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
