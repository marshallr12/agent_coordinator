//! The managed instruction block and the text surgery that places it.
//!
//! All functions here work on text with `\n` line endings and no byte-order
//! mark; [`decode`] and [`encode`] convert to and from a file's real bytes so
//! its line endings, BOM and trailing-newline habit survive an edit.

/// The instruction block `adopt` manages, markers included.
///
/// This is the text of the design chapter's "Instruction files" section; a
/// test keeps the two identical. It is this repository's own `AGENTS.md` plus
/// two facts a new session otherwise learns by failing: the info response is
/// too large to load whole, and the CLI needs a session name.
pub const BLOCK: &str = "\
<!-- agent-coordinator:begin v1 -->
## Agent Coordinator

This repository's work is coordinated by an Agent Coordinator service.
Read `.agent-coordinator.toml` and anonymously fetch the service's
`/api/v1/info` over HTTPS without redirects. The response is large: save it to
a temporary file and read `data.agent_startup.guide` one section at a time.
Follow that guide and same-origin authentication help, preferring an available
authenticated MCP connection; otherwise use the `agent-coordinator` CLI with a
session name unique to this harness (`AGENT_COORDINATOR_SESSION`).
Automatically select and claim eligible work unless the user changes the
scope. Keep credentials outside the repository and never expose them. Report a
concrete blocker if neither MCP nor the CLI is usable; do not select work from
local files.
<!-- agent-coordinator:end -->";

/// Every begin marker starts with this, whatever its version number.
const BEGIN_PREFIX: &str = "<!-- agent-coordinator:begin v";
/// The end marker, which carries no version.
const END_MARKER: &str = "<!-- agent-coordinator:end -->";
/// A file mentioning this already tells agents about the coordinator.
const BINDING_MENTION: &str = ".agent-coordinator.toml";
/// The UTF-8 byte-order mark some editors put at the start of a file.
const BOM: &str = "\u{feff}";

/// The byte-level habits of a file that an edit must preserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    bom: bool,
    crlf: bool,
    trailing_newline: bool,
    empty: bool,
}

/// The result of placing the block into a file's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge {
    /// No block was present; it was inserted at the top.
    Inserted(String),
    /// A block was present; the text between its markers was replaced.
    Replaced(String),
    /// No block, but this line already points agents at the binding.
    AlreadyMentions(String),
    /// A begin marker without its end marker: the file is left for a person.
    Unterminated,
}

/// Converts file bytes to `\n`-only text and records the file's habits; `None` if not UTF-8.
pub fn decode(bytes: &[u8]) -> Option<(String, Shape)> {
    let raw = std::str::from_utf8(bytes).ok()?;
    let body = raw.strip_prefix(BOM).unwrap_or(raw);
    let shape = Shape {
        bom: raw.starts_with(BOM),
        crlf: body.contains("\r\n"),
        trailing_newline: body.ends_with('\n'),
        empty: body.is_empty(),
    };
    Some((body.replace("\r\n", "\n"), shape))
}

/// Converts edited text back to bytes in the file's original habits.
pub fn encode(text: &str, shape: &Shape) -> Vec<u8> {
    let mut body = text.to_owned();
    if !shape.trailing_newline && !shape.empty && body.ends_with('\n') {
        body.pop();
    }
    if shape.crlf {
        body = body.replace('\n', "\r\n");
    }
    let prefix = if shape.bom { BOM } else { "" };
    format!("{prefix}{body}").into_bytes()
}

/// Places the block into `text`, following the rerun rules.
///
/// Markers present: replace between them. Markers absent but the file already
/// mentions the binding: leave it alone unless `force`. Otherwise insert at
/// the top.
pub fn merge_block(text: &str, force: bool) -> Merge {
    match managed_region(text) {
        Region::Whole(start, end) => {
            Merge::Replaced(format!("{}{BLOCK}{}", &text[..start], &text[end..]))
        }
        Region::Unterminated => Merge::Unterminated,
        Region::Absent => match mention(text) {
            Some(line) if !force => Merge::AlreadyMentions(line),
            _ => Merge::Inserted(insert_block(text)),
        },
    }
}

/// True when `text` holds the current block, complete and unmodified.
pub fn has_intact_block(text: &str) -> bool {
    text.contains(BLOCK)
}

/// Where the managed block sits in a text.
enum Region {
    Absent,
    /// Byte offsets from the begin marker's first byte to the end marker's last.
    Whole(usize, usize),
    Unterminated,
}

/// Finds the begin and end marker lines.
fn managed_region(text: &str) -> Region {
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end();
        if start.is_none() && is_begin_marker(content) {
            start = Some(offset);
        } else if let (Some(start), true) = (start, content == END_MARKER) {
            return Region::Whole(start, offset + content.len());
        }
        offset += line.len();
    }
    start.map_or(Region::Absent, |_| Region::Unterminated)
}

/// Recognises a begin marker of any version, so a newer block replaces an older one.
fn is_begin_marker(line: &str) -> bool {
    line.strip_prefix(BEGIN_PREFIX)
        .and_then(|rest| rest.strip_suffix(" -->"))
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()))
}

/// Returns the first line that already mentions the binding file.
fn mention(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains(BINDING_MENTION))
        .map(|line| line.trim().to_owned())
}

/// Inserts the block after any YAML front matter and leading `# ` title.
fn insert_block(text: &str) -> String {
    let (head, rest) = text.split_at(head_length(text));
    let rest = rest.trim_start_matches('\n');
    let mut merged = String::new();
    if !head.is_empty() {
        merged.push_str(head.trim_end_matches('\n'));
        merged.push_str("\n\n");
    }
    merged.push_str(BLOCK);
    merged.push('\n');
    if !rest.is_empty() {
        merged.push('\n');
        merged.push_str(rest);
    }
    merged
}

/// Counts the bytes of front matter and title that must stay above the block.
fn head_length(text: &str) -> usize {
    let front = front_matter_length(text);
    front + title_length(&text[front..])
}

/// Counts the bytes of a leading `---` ... `---` YAML block, or 0 when there is none.
fn front_matter_length(text: &str) -> usize {
    if !text.starts_with("---\n") {
        return 0;
    }
    let mut offset = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        if index > 0 && line.trim_end() == "---" {
            return offset;
        }
    }
    0
}

/// Counts the bytes up to the end of a leading `# ` title line, or 0 when there is none.
fn title_length(text: &str) -> usize {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.starts_with("# ") {
            return offset;
        }
        if !line.trim().is_empty() {
            return 0;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design chapter, whose block text the constant must match exactly.
    const CHAPTER: &str = include_str!("../../../../book/src/docs/project-scaffolding-design.md");

    #[test]
    fn the_block_is_the_text_in_the_design_chapter() {
        assert!(
            CHAPTER.contains(BLOCK),
            "BLOCK drifted from the design chapter"
        );
    }

    #[test]
    fn the_block_names_every_term_of_the_startup_contract() {
        for term in [
            ".agent-coordinator.toml",
            "/api/v1/info",
            "data.agent_startup.guide",
            "without redirects",
            "authenticated MCP connection",
            "session name",
            "AGENT_COORDINATOR_SESSION",
            "credentials outside the repository",
            "do not select work from\nlocal files",
        ] {
            assert!(BLOCK.contains(term), "the block must name {term:?}");
        }
    }

    #[test]
    fn the_block_goes_below_front_matter_and_title() {
        let Merge::Inserted(merged) = merge_block("---\na: 1\n---\n# Title\nBody.\n", false) else {
            panic!("expected an insertion");
        };
        assert_eq!(
            merged,
            format!("---\na: 1\n---\n# Title\n\n{BLOCK}\n\nBody.\n")
        );
        let Merge::Inserted(plain) = merge_block("Body.\n", false) else {
            panic!("expected an insertion");
        };
        assert_eq!(plain, format!("{BLOCK}\n\nBody.\n"));
        let Merge::Inserted(empty) = merge_block("", false) else {
            panic!("expected an insertion");
        };
        assert_eq!(empty, format!("{BLOCK}\n"));
    }

    #[test]
    fn an_older_block_is_upgraded_in_place() {
        let old = "Intro.\n<!-- agent-coordinator:begin v0 -->\nold words\n<!-- agent-coordinator:end -->\nOutro.\n";
        assert_eq!(
            merge_block(old, false),
            Merge::Replaced(format!("Intro.\n{BLOCK}\nOutro.\n"))
        );
        let current = format!("{BLOCK}\n");
        assert_eq!(
            merge_block(&current, false),
            Merge::Replaced(current.clone())
        );
    }

    #[test]
    fn a_missing_end_marker_is_not_guessed() {
        let broken = "<!-- agent-coordinator:begin v1 -->\nhalf a block\n";
        assert_eq!(merge_block(broken, false), Merge::Unterminated);
        assert_eq!(merge_block(broken, true), Merge::Unterminated);
    }

    #[test]
    fn existing_instructions_block_insertion_unless_forced() {
        let own = "Rules.\n  Read `.agent-coordinator.toml` first.  \n";
        assert_eq!(
            merge_block(own, false),
            Merge::AlreadyMentions("Read `.agent-coordinator.toml` first.".to_owned())
        );
        assert!(matches!(merge_block(own, true), Merge::Inserted(_)));
    }

    #[test]
    fn crlf_bom_and_missing_final_newline_survive_a_round_trip() {
        let original = "\u{feff}# Title\r\n\r\nBody without newline";
        let (text, shape) = decode(original.as_bytes()).unwrap();
        assert_eq!(text, "# Title\n\nBody without newline");
        let Merge::Inserted(merged) = merge_block(&text, false) else {
            panic!("expected an insertion");
        };
        let written = String::from_utf8(encode(&merged, &shape)).unwrap();
        assert!(
            written.starts_with("\u{feff}# Title\r\n\r\n<!-- agent-coordinator:begin v1 -->\r\n")
        );
        assert!(
            written.ends_with("\r\n\r\nBody without newline"),
            "{written:?}"
        );
        assert!(
            !written.replace("\r\n", "").contains('\n'),
            "only CRLF line endings"
        );
    }

    #[test]
    fn a_title_only_file_without_newline_keeps_that_habit() {
        let (text, shape) = decode(b"# Title").unwrap();
        let Merge::Inserted(merged) = merge_block(&text, false) else {
            panic!("expected an insertion");
        };
        let written = String::from_utf8(encode(&merged, &shape)).unwrap();
        assert_eq!(written, format!("# Title\n\n{BLOCK}"));
        assert!(
            decode(&[0xff, 0xfe, 0x00]).is_none(),
            "non-UTF-8 is refused"
        );
    }
}
