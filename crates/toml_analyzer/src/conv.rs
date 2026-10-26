//! Conversions between protocol types and IDE types.

use std::{path::PathBuf, str::FromStr};

use lsp_types::{Position, Range, Uri};
use toml_ide::{Document, LineCol, TextRange, TextSize};

/// Converts a `file://` URI into a path.
pub(crate) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

/// Converts a URL of the `url` crate into a protocol URI.
pub(crate) fn url_to_uri(url: &url::Url) -> Option<Uri> {
    Uri::from_str(url.as_str()).ok()
}

pub(crate) fn offset(doc: &Document, pos: Position) -> TextSize {
    doc.line_index.offset(&doc.text, LineCol { line: pos.line, col: pos.character })
}

pub(crate) fn position(doc: &Document, offset: TextSize) -> Position {
    let lc = doc.line_index.line_col(&doc.text, offset);
    Position::new(lc.line, lc.col)
}

pub(crate) fn range(doc: &Document, range: TextRange) -> Range {
    Range::new(position(doc, range.start()), position(doc, range.end()))
}

pub(crate) fn text_range(doc: &Document, range: Range) -> TextRange {
    let start = offset(doc, range.start);
    let end = offset(doc, range.end).max(start);
    TextRange::new(start, end)
}
