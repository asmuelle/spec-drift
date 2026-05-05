use crate::domain::{ClaimKind, Location, SpecClaim};
use crate::error::SpecDriftError;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::path::Path;

pub struct MarkdownParser;

impl MarkdownParser {
    /// Parse a Markdown file and extract both inline code spans and fenced
    /// code blocks as [`SpecClaim`]s. Each claim carries its source line
    /// for precise drift-report anchors.
    ///
    /// Fenced code blocks tagged with `rust`, `rs`, or untagged are parsed
    /// as code claims; untagged blocks are included because Rust projects
    /// often omit language tags.
    pub fn parse(path: &Path) -> Result<Vec<SpecClaim>, SpecDriftError> {
        let source = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut claims = Vec::new();
        let parser = Parser::new_ext(&source, Options::all()).into_offset_iter();

        for (event, range) in parser {
            match event {
                // Inline code spans: `code`
                Event::Code(text) => {
                    let line = line_of_offset(&source, range.start);
                    claims.push(SpecClaim {
                        location: Location::new(path, line),
                        kind: ClaimKind::Symbol,
                        text: text.to_string(),
                    });
                }
                // Fenced code blocks: ```rust ... ```
                Event::Start(Tag::CodeBlock(ref kind)) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(lang) if !lang.is_empty() => lang.to_string(),
                        CodeBlockKind::Fenced(_) => {
                            // Untagged fenced block — include as generic code
                            String::new()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    let line = line_of_offset(&source, range.start);
                    claims.push(SpecClaim {
                        location: Location::new(path, line),
                        kind: ClaimKind::Symbol,
                        text: if lang.is_empty() {
                            "[fenced code block]".to_string()
                        } else {
                            format!("[fenced code block: {lang}]")
                        },
                    });
                }
                // Text content inside a fenced code block — extract individual code lines
                Event::Text(ref text) => {
                    // pulldown-cmark emits Text events for code block content
                    // but we can't distinguish them from regular text without
                    // tracking parser state. For now, we capture code blocks
                    // via the Start/End tags and their content.
                    let _ = text;
                }
                _ => {}
            }
        }

        Ok(claims)
    }
}

/// Extended parser that also captures the full content of fenced code blocks.
pub struct MarkdownBlocks;

impl MarkdownBlocks {
    /// Extract fenced code blocks with their language tag and full content.
    /// Returns `Vec<(language, content)>` where language is empty string for untagged.
    pub fn extract_code_blocks(path: &Path) -> Result<Vec<(String, String)>, SpecDriftError> {
        let source = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut blocks = Vec::new();
        let parser = Parser::new_ext(&source, Options::all()).into_offset_iter();

        let mut current_lang = String::new();
        let mut current_content = String::new();
        let mut in_code_block = false;

        for (event, _range) in parser {
            match event {
                Event::Start(Tag::CodeBlock(ref kind)) => {
                    in_code_block = true;
                    current_content.clear();
                    current_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                Event::End(TagEnd::CodeBlock) => {
                    if in_code_block {
                        blocks.push((current_lang.clone(), current_content.clone()));
                        current_content.clear();
                        in_code_block = false;
                    }
                }
                Event::Text(text) => {
                    if in_code_block {
                        current_content.push_str(&text);
                    }
                }
                _ => {}
            }
        }

        Ok(blocks)
    }
}

fn line_of_offset(src: &str, offset: usize) -> u32 {
    let end = offset.min(src.len());
    (src[..end].bytes().filter(|&b| b == b'\n').count() as u32) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_inline_code_spans_with_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("README.md");
        std::fs::write(&path, "# Title\n\nUse `Client::new()` to connect.\n").unwrap();

        let claims = MarkdownParser::parse(&path).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].text, "Client::new()");
        assert_eq!(claims[0].location.line, 3);
    }

    #[test]
    fn extracts_fenced_code_block_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("README.md");
        std::fs::write(&path, "# API\n\n```rust\nfn main() {}\n```\n").unwrap();

        let claims = MarkdownParser::parse(&path).unwrap();
        let has_fenced = claims
            .iter()
            .any(|c| c.text.contains("fenced code block: rust"));
        assert!(
            has_fenced,
            "should extract fenced code block tag, got {:?}",
            claims
        );
    }

    #[test]
    fn extracts_full_code_block_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("README.md");
        std::fs::write(
            &path,
            "# Example\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n",
        )
        .unwrap();

        let blocks = MarkdownBlocks::extract_code_blocks(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "rust");
        assert!(blocks[0].1.contains("println!"));
    }

    #[test]
    fn multiple_fenced_blocks_extracted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("README.md");
        std::fs::write(
            &path,
            "```sh\ncargo build\n```\n\n```rust\nlet x = 1;\n```\n",
        )
        .unwrap();

        let blocks = MarkdownBlocks::extract_code_blocks(&path).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "sh");
        assert_eq!(blocks[1].0, "rust");
    }
}
