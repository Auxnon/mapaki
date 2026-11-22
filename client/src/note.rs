//! `.note` files: small TOML documents with images embedded as
//! base64, so a note travels (and versions, and merges) as one text file.
//!
//! Every client writes the same canonical layout, field order and quoting,
//! so two devices editing different parts of a note produce a clean 3-way
//! merge on the server instead of a conflict. Reading is lenient: any valid
//! TOML works, and a plain text file opens as a note body. `.link` files use
//! the same conventions; see the README.
//!
//! ```toml
//! title = "Groceries"
//! created = "2026-11-22T18:00:00Z"
//! body = """
//! Milk, eggs.
//! ![](receipt.jpg)
//! """
//!
//! [[images]]
//! name = "receipt.jpg"
//! type = "image/jpeg"
//! data = "/9j/4AAQ..."
//! ```

use data_encoding::BASE64;
use toml::{Table, Value};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Image {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Note {
    pub title: String,
    pub created: String,
    /// Plain text; a line that is just `![alt](name)` shows that image there.
    pub body: String,
    pub images: Vec<Image>,
}

fn text(t: &Table, key: &str) -> String {
    match t.get(key) {
        Some(Value::String(s)) => s.clone(),
        // Hand-written files may use a real TOML datetime for `created`.
        Some(Value::Datetime(d)) => d.to_string(),
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Float(n)) => n.to_string(),
        Some(Value::Boolean(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn image(t: &Table) -> Option<Image> {
    let data = text(t, "data");
    // Tolerate line-wrapped base64 from other tools.
    let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    Some(Image {
        name: text(t, "name"),
        mime: Some(text(t, "type")).filter(|m| !m.is_empty()).unwrap_or_else(|| "image/jpeg".into()),
        data: BASE64.decode(data.as_bytes()).ok()?,
    })
}

impl Note {
    pub fn parse(bytes: &[u8]) -> Note {
        let src = String::from_utf8_lossy(bytes);
        match src.parse::<Table>() {
            Ok(t) if t.contains_key("body") || t.contains_key("title") => Note {
                title: text(&t, "title"),
                created: text(&t, "created"),
                body: text(&t, "body"),
                images: match t.get("images") {
                    Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_table().and_then(image)).collect(),
                    _ => vec![],
                },
            },
            // Not ours (or not TOML at all): show it as the body rather than nothing.
            _ => Note { body: src.into_owned(), ..Note::default() },
        }
    }

    /// Title to show: the explicit one, else the first non-empty body line.
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        self.body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && image_ref(l).is_none())
            .map(|l| l.chars().take(60).collect())
            .unwrap_or_else(|| "(untitled)".into())
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("title = {}\n", basic(&self.title)));
        if !self.created.is_empty() {
            out.push_str(&format!("created = {}\n", basic(&self.created)));
        }
        out.push_str(&format!("body = {}\n", multiline(&self.body)));
        for img in &self.images {
            out.push_str("\n[[images]]\n");
            write_image(&mut out, img);
        }
        out
    }
}

/// `![alt](name)` on a line of its own -> `name`.
pub fn image_ref(line: &str) -> Option<&str> {
    let line = line.trim();
    let rest = line.strip_prefix("![")?;
    let close = rest.find("](")?;
    let name = rest[close + 2..].strip_suffix(')')?;
    (!name.is_empty()).then_some(name)
}

fn write_image(out: &mut String, img: &Image) {
    out.push_str(&format!("name = {}\n", basic(&img.name)));
    out.push_str(&format!("type = {}\n", basic(&img.mime)));
    out.push_str(&format!("data = \"{}\"\n", BASE64.encode(&img.data)));
}

fn escape(s: &str, multiline: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' if !multiline => out.push_str("\\\""),
            '\n' if multiline => out.push('\n'),
            '\n' => out.push_str("\\n"),
            '\t' => out.push('\t'),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn basic(s: &str) -> String {
    format!("\"{}\"", escape(s, false))
}

/// Multi-line basic string, one body line per file line so merges work line by line.
/// The body is normalized to end in exactly one newline.
fn multiline(s: &str) -> String {
    let body = s.trim_end_matches(['\n', '\r']);
    if body.is_empty() {
        return "\"\"\"\n\"\"\"".into();
    }
    // Quotes only need escaping where three would close the string, or
    // where one ends the body right before the closing delimiter.
    let mut esc = escape(body, true).replace("\"\"\"", "\"\"\\\"");
    if esc.ends_with('"') {
        esc.pop();
        esc.push_str("\\\"");
    }
    format!("\"\"\"\n{esc}\n\"\"\"")
}

/// UTC now as RFC 3339, e.g. `2026-11-22T18:00:00Z`.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (date, time) = crate::civil(secs);
    format!("{date}T{time}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Note {
        Note {
            title: "Groceries \"weekly\"".into(),
            created: "2026-11-22T18:00:00Z".into(),
            body: "Milk\n\"\"\" not the end\nback\\slash\ttab\n![receipt](r.jpg)\nends with \"".into(),
            images: vec![Image { name: "r.jpg".into(), mime: "image/jpeg".into(), data: vec![0, 1, 2, 255] }],
        }
    }

    #[test]
    fn note_round_trips() {
        let n = sample();
        let back = Note::parse(n.to_toml().as_bytes());
        assert_eq!(back.title, n.title);
        assert_eq!(back.created, n.created);
        assert_eq!(back.body, format!("{}\n", n.body));
        assert_eq!(back.images, n.images);
    }

    /// The Android app tests against the same file (NoteFormatTest.kt), so
    /// both clients provably write identical bytes for the same note.
    #[test]
    fn matches_shared_fixture() {
        let fixture = include_str!("../../fixtures/sample.note");
        let n = Note {
            title: "Groceries \"weekly\"".into(),
            created: "2026-11-22T18:00:00Z".into(),
            body: "Milk\n\"\"\" not the end\nback\\slash\ttab\nünïcode ✓\n![receipt](r.jpg)\nends with \"".into(),
            images: vec![Image { name: "r.jpg".into(), mime: "image/jpeg".into(), data: vec![0, 1, 2, 255] }],
        };
        assert_eq!(n.to_toml(), fixture);
        assert_eq!(Note::parse(fixture.as_bytes()).to_toml(), fixture);
    }

    #[test]
    fn canonical_layout_is_stable() {
        let n = Note { title: "T".into(), body: "a\nb".into(), ..Note::default() };
        assert_eq!(n.to_toml(), "title = \"T\"\nbody = \"\"\"\na\nb\n\"\"\"\n");
        // Re-encoding a parsed note doesn't drift.
        let once = sample().to_toml();
        assert_eq!(Note::parse(once.as_bytes()).to_toml(), once);
    }

    #[test]
    fn plain_text_still_opens() {
        let n = Note::parse(b"just some text\n");
        assert_eq!(n.body, "just some text\n");
        assert_eq!(n.display_title(), "just some text");
    }

    #[test]
    fn image_refs() {
        assert_eq!(image_ref("![receipt](r.jpg)"), Some("r.jpg"));
        assert_eq!(image_ref("  ![](a b.png) "), Some("a b.png"));
        assert_eq!(image_ref("see ![](x.png)"), None);
        assert_eq!(image_ref("![]()"), None);
    }
}
