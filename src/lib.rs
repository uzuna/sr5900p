pub mod label;
pub mod printer;

// compiles the README examples as doctests so that they cannot drift from the API
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;

use crate::label::error::LabelError;
use crate::label::mm_to_px;
use crate::printer::PrinterError;

/// Either side of rendering and printing a label, for callers that do both.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Label(#[from] LabelError),
    #[error(transparent)]
    Printer(#[from] PrinterError),
    #[error(transparent)]
    TapeWidth(#[from] UnknownTapeWidth),
}

#[derive(Debug, thiserror::Error)]
#[error("tape for {0} mm is not defined")]
pub struct UnknownTapeWidth(pub usize);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tape {
    W4,
    W6,
    W9,
    W12,
    W18,
    W24,
    W36,
}
impl Tape {
    pub fn from_mm(mm: usize) -> Result<Self, UnknownTapeWidth> {
        Ok(match mm {
            4 => Tape::W4,
            6 => Tape::W6,
            9 => Tape::W9,
            12 => Tape::W12,
            18 => Tape::W18,
            24 => Tape::W24,
            36 => Tape::W36,
            _ => return Err(UnknownTapeWidth(mm)),
        })
    }
    pub fn width_px(&self) -> i32 {
        let w = match self {
            Tape::W4 => 2.85,  // verified
            Tape::W6 => 5.0,   // verified
            Tape::W9 => 7.0,   // verified
            Tape::W12 => 10.0, // verified
            Tape::W18 => 15.2, // verified
            Tape::W24 => 20.0, // verified
            Tape::W36 => 26.0, // verified
        };
        let w = mm_to_px(w);
        // tape width in px should be multiple of 8
        (w + 7) / 8 * 8
    }
}
