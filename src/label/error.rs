use crate::Tape;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LabelError {
    #[error("failed to parse template: {0}")]
    InvalidTemplate(String),
    #[error("field '{0}' is required by the template")]
    MissingField(String),
    #[error("unclosed '{{' in {0:?}")]
    UnclosedPlaceholder(String),
    #[error("font '{0}' is not defined in [fonts]")]
    UnknownFont(String),
    #[error("logo '{0}' is not registered")]
    UnknownLogo(String),
    #[error("no logo was given for slot '{0}' and the column has no default")]
    MissingLogo(String),
    #[error("label is {length_mm:.1} mm long, which exceeds max_length_mm = {max_mm}")]
    TooLong { length_mm: f32, max_mm: f32 },
    #[error("failed to generate a {kind}: {reason}")]
    Encode { kind: CodeKind, reason: String },
    /// the cause is in `source`, not repeated in the message
    #[error("failed to read {what} {}", path.display())]
    Io {
        /// what was being read ("template", "font", "image")
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to load font {}: {reason}", path.display())]
    Font { path: PathBuf, reason: String },
    #[error("failed to load image {}: {reason}", path.display())]
    Image { path: PathBuf, reason: String },
    #[error("failed to encode PNG: {0}")]
    Png(String),
    #[error("the template is for {template:?} tape, not {requested:?}")]
    TemplateTapeMismatch { template: Tape, requested: Tape },
    #[error("the bitmap is {height} px high but {tape:?} tape takes {} px", tape.width_px())]
    HeightMismatch { height: usize, tape: Tape },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodeKind {
    Qr,
    DataMatrix,
    Code128,
}

impl fmt::Display for CodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CodeKind::Qr => "QR code",
            CodeKind::DataMatrix => "Data Matrix",
            CodeKind::Code128 => "Code128 barcode",
        })
    }
}
