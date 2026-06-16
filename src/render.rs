//! Produce the read-only view text for a file, by kind. Each renderer returns
//! a ready-to-display String (may carry SGR styling via crust `style`). External
//! tools (pandoc, pdftotext) are only spawned for their formats — cold paths.

use std::path::Path;
use std::process::Command;

use crust::style;

use crate::registry::Kind;

pub fn render(path: &Path, kind: Kind) -> String {
    match kind {
        Kind::Text => render_text(path),
        Kind::Table => render_table(path),
        Kind::Doc => render_doc(path),
        Kind::Slides => render_slides(path),
        Kind::Pdf => render_pdf(path),
        Kind::Image => String::new(), // drawn inline by glow, not as text
        Kind::Hex => render_hex(path),
    }
}

const ALL: usize = usize::MAX; // highlight the whole file (viewer scrolls)

/// Read a text file and syntax-highlight it by extension via fe2o3-highlight.
/// HyperList tabs render at width 3 (matches pointer/scribe).
fn render_text(path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return render_hex(path),
    };
    match ext(path).as_str() {
        "hl" => highlight::highlight_hyperlist(&content.replace('\t', "   "), ALL),
        "md" | "markdown" => highlight::highlight_markdown(&content, ALL),
        "tex" | "latex" | "ltx" | "sty" | "cls" | "bib" => highlight::highlight_tex(&content, ALL),
        "txt" | "text" | "log" | "readme" => highlight::highlight_text(&content, ALL),
        e if highlight::lang_known(e).is_some() => highlight::highlight(&content, e, ALL),
        _ => highlight::highlight_text(&content, ALL),
    }
}

fn render_doc(path: &Path) -> String {
    match run("pandoc", &["-t", "markdown", "--wrap=none", &path.to_string_lossy()]) {
        Ok(s) if !s.trim().is_empty() => highlight::highlight_markdown(&s, ALL),
        Ok(_) => "(empty document)".into(),
        Err(e) => format!("pandoc failed: {}\n(install pandoc to view .docx/.odt)", e),
    }
}

fn render_pdf(path: &Path) -> String {
    match run("pdftotext", &["-layout", &path.to_string_lossy(), "-"]) {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => "(no extractable text — scanned PDF?)".into(),
        Err(e) => format!("pdftotext failed: {}\n(install poppler-utils to view PDFs)", e),
    }
}

// ---- slides (pptx / odp) ------------------------------------------------

/// Text outline of a presentation. pptx and odp aren't pandoc inputs, so we
/// unzip and pull the text runs out of the slide XML.
fn render_slides(path: &Path) -> String {
    let p = path.to_string_lossy().to_string();
    if ext(path) == "odp" {
        return render_odp(&p);
    }
    let names = match run("unzip", &["-Z1", &p]) {
        Ok(s) => s,
        Err(e) => return format!("unzip failed: {}\n(install unzip to view .pptx)", e),
    };
    let mut slides: Vec<&str> = names
        .lines()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .collect();
    slides.sort_by_key(|n| slide_num(n));
    if slides.is_empty() {
        return "(no slides found)".into();
    }
    let mut out = String::new();
    for (i, name) in slides.iter().enumerate() {
        let xml = run("unzip", &["-p", &p, name]).unwrap_or_default();
        out.push_str(&slide_header(i + 1));
        out.push('\n');
        let text = extract_runs(&xml, "a:t", "</a:p>");
        out.push_str(if text.trim().is_empty() { "(no text)" } else { &text });
        out.push_str("\n\n");
    }
    out
}

/// The "── Slide N ──" divider, shared by pptx and odp.
fn slide_header(n: usize) -> String {
    style::coded(&format!("\u{2500}\u{2500} Slide {} \u{2500}\u{2500}", n), "11,,b")
}

fn slide_num(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(0)
}

/// ODF presentation: each `<draw:page>` in content.xml is a slide. Split on
/// those so odp gets the same "── Slide N ──" dividers as pptx; within a slide,
/// text lives in `<text:p>`/`<text:h>` (tags stripped).
fn render_odp(p: &str) -> String {
    let xml = run("unzip", &["-p", p, "content.xml"]).unwrap_or_default();
    if xml.is_empty() {
        return "(could not read content.xml)".into();
    }
    let mut out = String::new();
    let mut n = 0;
    // Split on "<draw:page " (trailing space) so we don't also match
    // <draw:page-thumbnail>; then skip the opening tag's own attributes.
    for part in xml.split("<draw:page ").skip(1) {
        let after_tag = match part.find('>') {
            Some(i) => &part[i + 1..],
            None => part,
        };
        let slide = after_tag.split("</draw:page>").next().unwrap_or("");
        n += 1;
        out.push_str(&slide_header(n));
        out.push('\n');
        let body = slide.replace("</text:p>", "\n").replace("</text:h>", "\n");
        let stripped = strip_tags(&body);
        let mut any = false;
        for line in stripped.lines() {
            let t = line.trim();
            if !t.is_empty() {
                out.push_str(t);
                out.push('\n');
                any = true;
            }
        }
        if !any {
            out.push_str("(no text)\n");
        }
        out.push('\n');
    }
    if n == 0 {
        return strip_tags(&xml.replace("</text:p>", "\n"));
    }
    out
}

/// Collect the text inside each `<tag>…</tag>` run, grouped into lines by the
/// paragraph-close marker. Handles tags carrying attributes (e.g. xml:space).
fn extract_runs(xml: &str, tag: &str, para_close: &str) -> String {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut lines = Vec::new();
    for chunk in xml.split(para_close) {
        let mut s = String::new();
        let mut rest = chunk;
        while let Some(i) = rest.find(&open) {
            let after = &rest[i + open.len()..];
            let Some(gt) = after.find('>') else { break };
            let content = &after[gt + 1..];
            let Some(j) = content.find(&close) else { break };
            s.push_str(&content[..j]);
            rest = &content[j + close.len()..];
        }
        let t = unescape(s.trim());
        if !t.is_empty() {
            lines.push(t);
        }
    }
    lines.join("\n")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out)
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn render_hex(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut out = String::from("(binary file — first 2 KB)\n\n");
            for (i, chunk) in bytes.chunks(16).take(128).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
                let ascii: String = chunk
                    .iter()
                    .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                    .collect();
                out.push_str(&format!("{:08x}  {:<47}  {}\n", i * 16, hex.join(" "), ascii));
            }
            out
        }
        Err(e) => format!("cannot read file: {}", e),
    }
}

// ---- table (csv / xlsx / ods) -------------------------------------------

fn render_table(path: &Path) -> String {
    let rows = match ext(path).as_str() {
        "csv" | "tsv" => read_csv(path, if ext(path) == "tsv" { '\t' } else { ',' }),
        _ => match read_spreadsheet(path) {
            Ok(r) => r,
            Err(e) => return format!("cannot read spreadsheet: {}", e),
        },
    };
    format_table(&rows)
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn read_csv(path: &Path, sep: char) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().map(|l| split_line(l, sep)).collect()
}

/// Quote-aware split (RFC4180-ish: `""` escapes a quote).
fn split_line(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_q && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = !in_q;
                }
            }
            c if c == sep && !in_q => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn read_spreadsheet(path: &Path) -> Result<Vec<Vec<String>>, String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let name = wb.sheet_names().first().cloned().ok_or("no sheets")?;
    let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for row in range.rows() {
        rows.push(row.iter().map(cell_text).collect());
    }
    Ok(rows)
}

fn cell_text(d: &calamine::Data) -> String {
    use calamine::Data as D;
    match d {
        D::Empty => String::new(),
        D::String(s) => s.clone(),
        D::Float(f) => {
            if *f == f.trunc() && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{}", f)
            }
        }
        D::Int(i) => i.to_string(),
        D::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        D::DateTime(dt) => excel_serial_to_string(dt.as_f64()),
        D::DateTimeIso(s) => s.clone(),
        other => format!("{}", other),
    }
}

/// Excel date serial → readable `YYYY-MM-DD` (with `HH:MM` for a time part).
fn excel_serial_to_string(serial: f64) -> String {
    let whole = serial.floor();
    let frac = serial - whole;
    let secs = (frac * 86_400.0).round() as i64;
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if whole < 1.0 {
        return format!("{:02}:{:02}", h, m);
    }
    let (y, mo, d) = civil_from_days(whole as i64 - 25_569); // 25569 = 1970-01-01
    if secs > 0 {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, m)
    } else {
        format!("{:04}-{:02}-{:02}", y, mo, d)
    }
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MAX_COL: usize = 28; // cap a column's display width

/// Lay rows out as an aligned table; the first row is bolded as a header.
fn format_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return "(empty)".into();
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(crust::display_width(cell).min(MAX_COL));
        }
    }
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for c in 0..ncols {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            let fitted = fit(cell, widths[c]);
            line.push_str(&fitted);
            line.push_str("  ");
        }
        let line = line.trim_end();
        if r == 0 {
            out.push_str(&style::coded(line, ",,b"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Pad/truncate to exactly `w` display columns (… on overflow).
fn fit(s: &str, w: usize) -> String {
    let sw = crust::display_width(s);
    if sw == w {
        s.to_string()
    } else if sw < w {
        format!("{}{}", s, " ".repeat(w - sw))
    } else {
        format!("{}\u{2026}", crust::truncate_ansi(s, w.saturating_sub(1)))
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {}", cmd, e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
