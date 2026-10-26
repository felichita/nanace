//! Markdown chunking for embedding.
//!
//! Embedding models have a bounded input window (bge-small: 512 tokens), so a
//! context is split into chunks of at most `max_bytes`. Splits prefer heading
//! boundaries, then blank lines, then whitespace. Headings inside fenced code
//! blocks are not treated as boundaries.

/// Splits `text` into trimmed, non-empty chunks of at most `max_bytes` bytes.
///
/// # Panics
///
/// Panics if `max_bytes` is smaller than 4 (one UTF-8 scalar must always fit).
pub fn chunk_markdown(text: &str, max_bytes: usize) -> Vec<String> {
    assert!(max_bytes >= 4, "max_bytes must fit any UTF-8 character");

    let mut blocks = Vec::new();
    for section in split_sections(text) {
        if section.len() <= max_bytes {
            blocks.push(section);
        } else {
            for paragraph in section.split("\n\n") {
                hard_split(paragraph, max_bytes, &mut blocks);
            }
        }
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for block in blocks {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if !current.is_empty() && current.len() + 2 + block.len() > max_bytes {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(block);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Splits before every markdown heading line that is outside a code fence.
fn split_sections(text: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && trimmed.starts_with('#') && offset > start {
            sections.push(&text[start..offset]);
            start = offset;
        }
        offset += line.len();
    }
    if start < text.len() {
        sections.push(&text[start..]);
    }
    sections
}

/// Pushes `text` as pieces of at most `max_bytes`, cutting at the last
/// whitespace before the limit when possible and always on a char boundary.
fn hard_split<'a>(mut text: &'a str, max_bytes: usize, out: &mut Vec<&'a str>) {
    while text.len() > max_bytes {
        let mut cut = max_bytes;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(ws) = text[..cut].rfind(char::is_whitespace).filter(|&i| i > 0) {
            cut = ws;
        }
        out.push(&text[..cut]);
        text = text[cut..].trim_start();
    }
    out.push(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_input_yield_no_chunks() {
        assert!(chunk_markdown("", 100).is_empty());
        assert!(chunk_markdown(" \n\n \n", 100).is_empty());
    }

    #[test]
    fn small_document_is_one_chunk() {
        let doc = "## API Authentication\n\nPOST /auth/login\nReturns JWT token\n";
        assert_eq!(
            chunk_markdown(doc, 1000),
            vec!["## API Authentication\n\nPOST /auth/login\nReturns JWT token"]
        );
    }

    #[test]
    fn sections_split_at_headings_when_over_limit() {
        let doc = "# A\naaaa aaaa\n# B\nbbbb bbbb\n";
        assert_eq!(chunk_markdown(doc, 16), vec!["# A\naaaa aaaa", "# B\nbbbb bbbb"]);
    }

    #[test]
    fn headings_inside_code_fences_are_not_boundaries() {
        let doc = "# Script\n```sh\n# not a heading\necho hi\n```\n";
        assert_eq!(split_sections(doc).len(), 1);
    }

    #[test]
    fn every_chunk_respects_the_limit_and_char_boundaries() {
        let long_word = "é".repeat(100);
        let doc = format!("# T\n\n{long_word}\n\nshort para\n\n{}", "word ".repeat(60));
        for max in [4, 5, 7, 16, 64, 200] {
            let chunks = chunk_markdown(&doc, max);
            assert!(!chunks.is_empty());
            for chunk in &chunks {
                assert!(chunk.len() <= max, "chunk of {} bytes exceeds {max}", chunk.len());
                assert!(!chunk.trim().is_empty());
            }
        }
    }

    #[test]
    fn no_text_is_lost() {
        let doc = "# One\n\nalpha beta gamma\n\n## Two\n\ndelta epsilon\n\nzeta eta theta iota";
        let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        for max in [8, 20, 1000] {
            let joined = chunk_markdown(doc, max).join(" ");
            assert_eq!(words(&joined), words(doc), "max={max}");
        }
    }
}
