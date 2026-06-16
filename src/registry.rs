//! The format registry: extension → (view kind, edit command). Shipped with
//! sensible defaults, overridable/extendable via a plain config file so adding
//! a new format is a one-line edit, not a code change (the mapping is data, not
//! hardcoded `if ext == X`). User config: `~/.config/viewer/handlers.conf`,
//! lines of `ext  kind  edit-command` (`-` = no editor; `#` comments).

use std::collections::HashMap;
use std::path::Path;

/// How the file is rendered read-only in the TUI.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Text,   // read as UTF-8 text (md, txt, code, hyperlist…)
    Table,  // spreadsheet: csv natively, xlsx/ods via calamine
    Doc,    // word processor: docx/odt via pandoc → markdown
    Slides, // presentation: pptx/odp text outline (unzip + extract)
    Pdf,    // pdftotext
    Image,  // rendered inline via glow (kitty/sixel)
    Hex,    // fallback for binary / unknown
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "text" => Kind::Text,
            "table" => Kind::Table,
            "doc" => Kind::Doc,
            "slides" => Kind::Slides,
            "pdf" => Kind::Pdf,
            "image" => Kind::Image,
            "hex" => Kind::Hex,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Table => "table",
            Kind::Doc => "document",
            Kind::Slides => "slides",
            Kind::Pdf => "pdf",
            Kind::Image => "image",
            Kind::Hex => "binary",
        }
    }
}

#[derive(Clone)]
pub struct Handler {
    pub kind: Kind,
    /// Editor command line (without the file arg). `$VAR` expands from the
    /// environment ($EDITOR falls back to vim). None = view-only.
    pub edit: Option<String>,
}

pub struct Registry {
    map: HashMap<String, Handler>,
}

impl Registry {
    pub fn load() -> Self {
        let mut map = defaults();
        if let Some(home) = std::env::var_os("HOME") {
            let path = Path::new(&home).join(".config/viewer/handlers.conf");
            if let Ok(text) = std::fs::read_to_string(&path) {
                merge(&mut map, &text);
            }
        }
        Registry { map }
    }

    /// Look up the handler for a path by its extension. Unknown extensions fall
    /// back to Text (which itself degrades to a binary notice if not UTF-8).
    pub fn lookup(&self, path: &Path) -> Handler {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        self.map
            .get(&ext)
            .cloned()
            .unwrap_or(Handler { kind: Kind::Text, edit: edit("$EDITOR") })
    }
}

fn edit(cmd: &str) -> Option<String> {
    Some(cmd.to_string())
}

fn defaults() -> HashMap<String, Handler> {
    let mut m = HashMap::new();
    let mut add = |exts: &[&str], kind: Kind, e: Option<String>| {
        for x in exts {
            m.insert((*x).to_string(), Handler { kind, edit: e.clone() });
        }
    };
    add(&["csv", "tsv"], Kind::Table, edit("grid"));
    add(&["xlsx", "xlsm", "xlsb", "xls", "ods"], Kind::Table, edit("grid"));
    add(&["md", "markdown", "txt", "text", "hl"], Kind::Text, edit("scribe"));
    add(
        &["rs", "py", "sh", "bash", "js", "ts", "c", "h", "cpp", "rb", "go", "lua", "vim", "toml", "json", "yaml", "yml", "conf", "ini", "css", "html", "xml"],
        Kind::Text,
        edit("$EDITOR"),
    );
    add(&["docx", "odt"], Kind::Doc, None); // view-only until the AI-doc editor lands
    add(&["pptx", "odp"], Kind::Slides, None);
    add(&["pdf"], Kind::Pdf, None);
    add(&["png", "jpg", "jpeg", "gif", "webp", "bmp"], Kind::Image, None);
    m
}

/// Merge an override config: `ext  kind  edit...` per line.
fn merge(map: &mut HashMap<String, Handler>, text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(ext), Some(kind_s)) = (it.next(), it.next()) else { continue };
        let Some(kind) = Kind::parse(kind_s) else { continue };
        let rest = it.collect::<Vec<_>>().join(" ");
        let edit = if rest.is_empty() || rest == "-" { None } else { Some(rest) };
        map.insert(ext.to_ascii_lowercase(), Handler { kind, edit });
    }
}
