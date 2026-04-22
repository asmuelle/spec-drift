use crate::domain::{ClaimKind, Location, SpecClaim};
use crate::error::SpecDriftError;
use pulldown_cmark::{Event, Options, Parser};
use std::path::Path;

pub struct MarkdownParser;

impl MarkdownParser {
    /// Parse a Markdown file and extract every inline code span as a
    /// [`SpecClaim`]. Each claim carries its source line for precise
    /// drift-report anchors.
    pub fn parse(path: &Path) -> Result<Vec<SpecClaim>, SpecDriftError> {
        let source = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut claims = Vec::new();
        let parser = Parser::new_ext(&source, Options::all()).into_offset_iter();

        for (event, range) in parser {
            if let Event::Code(text) = event {
                let line = line_of_offset(&source, range.start);
                claims.push(SpecClaim {
                    location: Location::new(path, line),
                    kind: ClaimKind::Symbol,
                    text: text.to_string(),
                });
            }
        }

        Ok(claims)
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
}
