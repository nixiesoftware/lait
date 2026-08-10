//! Restricted Typst compilation for issue documents.
//!
//! The browser never receives or executes compiler-produced HTML. This module
//! is the validation boundary for the hidden document source: it exposes only
//! whether compilation succeeded and source-local diagnostic ranges. The
//! React client renders the controlled Lait vocabulary as typed nodes.

use serde::{Deserialize, Serialize};
use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Feature, Library, LibraryExt, World, WorldExt};
use typst_html::HtmlDocument;

use issues::contract::DOCUMENT_PREFIX;

/// The only definitions available in an issue document beyond Typst's
/// standard markup vocabulary. Their names are storage details and are never
/// presented by the viewer.
const PRELUDE: &str = r#"#let lait-callout(tone, body) = block[
  #strong(tone)
  #linebreak()
  #body
]
#let lait-task(checked, body) = [#if checked [☑] else [☐] #body]
#let lait-table(align: (), header: (), rows: ()) = table(
  columns: header.len(),
  table.header(..header),
  ..rows.flatten(),
)
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDiagnostic {
    /// UTF-8 byte range within the hidden issue source, when available.
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub message: String,
    pub warning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentCompilation {
    pub valid: bool,
    pub diagnostics: Vec<DocumentDiagnostic>,
}

/// Turn semantic plain text into current canonical source. This is used at the
/// protocol boundary so CLI/MCP callers never need to know Typst punctuation.
pub fn plain_document(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut escaped = String::with_capacity(DOCUMENT_PREFIX.len().saturating_add(normalized.len()));
    escaped.push_str(DOCUMENT_PREFIX);
    for character in normalized.chars() {
        if matches!(
            character,
            '\\' | '#' | '[' | ']' | '*' | '_' | '`' | '$' | '<' | '>' | '@'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Compile one controlled issue document in a hermetic Typst world.
///
/// The produced HTML document is intentionally discarded. Rendering
/// compiler HTML in the browser would create a second, unsafe DOM language and
/// make source-to-selection mapping dependent on unstable exporter details.
pub fn compile_document(body: &str) -> DocumentCompilation {
    let world = match DocumentWorld::new(body) {
        Ok(world) => world,
        Err(message) => {
            return DocumentCompilation {
                valid: false,
                diagnostics: vec![DocumentDiagnostic {
                    start: None,
                    end: None,
                    message,
                    warning: false,
                }],
            };
        }
    };

    let compiled = typst::compile::<HtmlDocument>(&world);
    let mut diagnostics = compiled
        .warnings
        .iter()
        .filter(|warning| warning.span.id() == Some(world.main.id()))
        .map(|warning| diagnostic(&world, warning))
        .collect::<Vec<_>>();
    let valid = match compiled.output {
        Ok(_) => true,
        Err(errors) => {
            diagnostics.extend(errors.iter().map(|error| diagnostic(&world, error)));
            false
        }
    };
    DocumentCompilation { valid, diagnostics }
}

fn diagnostic(world: &DocumentWorld, diagnostic: &SourceDiagnostic) -> DocumentDiagnostic {
    let range = world.range(diagnostic.span).and_then(|range| {
        (range.end >= world.body_offset).then(|| {
            range.start.saturating_sub(world.body_offset)
                ..range.end.saturating_sub(world.body_offset)
        })
    });
    DocumentDiagnostic {
        start: range.as_ref().map(|range| range.start),
        end: range.map(|range| range.end),
        message: diagnostic.message.to_string(),
        warning: diagnostic.severity == Severity::Warning,
    }
}

struct DocumentWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    main: Source,
    body_offset: usize,
}

impl DocumentWorld {
    fn new(body: &str) -> Result<Self, String> {
        let main_id = file_id("main.typ")?;
        let features = [Feature::Html].into_iter().collect();
        let mut source = String::with_capacity(PRELUDE.len().saturating_add(body.len()));
        source.push_str(PRELUDE);
        let body_offset = source.len();
        source.push_str(body);
        Ok(Self {
            library: LazyHash::new(Library::builder().with_features(features).build()),
            book: LazyHash::new(FontBook::new()),
            main: Source::new(main_id, source),
            body_offset,
        })
    }
}

fn file_id(path: &str) -> Result<FileId, String> {
    let path = VirtualPath::new(path).map_err(|error| error.to_string())?;
    Ok(RootedPath::new(VirtualRoot::Project, path).intern())
}

impl World for DocumentWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.source(id)
            .map(|source| Bytes::from_string(source.text().to_owned()))
    }

    fn font(&self, _index: usize) -> Option<Font> {
        None
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_the_controlled_document_vocabulary() {
        let source = r#"= Heading

Plain *strong* and _emphasized_ text.

#lait-callout("note", [Remember this.])

- #lait-task(true, [finished])
- #lait-task(false, [remaining])

#lait-table(
  align: ("left", "right"),
  header: ([Name], [Count]),
  rows: (
    ([alpha], [2]),
  ),
)
"#;
        let result = compile_document(source);
        assert!(result.valid, "{:?}", result.diagnostics);
    }

    #[test]
    fn reports_body_local_diagnostics_without_exposing_compiler_html() {
        let source = "A #missing-function()";
        let result = compile_document(source);
        assert!(!result.valid);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.start.is_some() && diagnostic.message.contains("missing-function")
        }));
    }

    #[test]
    fn cannot_read_files_outside_the_in_memory_source() {
        let result = compile_document("#include \"secrets.typ\"");
        assert!(!result.valid);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("access denied")));
    }

    #[test]
    fn semantic_plain_text_is_escaped_and_discriminated() {
        let source = plain_document("ordinary #text and *stars*");
        assert_eq!(
            source,
            "// lait-document:1\nordinary \\#text and \\*stars\\*"
        );
        assert!(compile_document(&source).valid);
    }
}
