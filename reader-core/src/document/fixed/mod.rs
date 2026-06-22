//! Fixed-layout document backends (trivial integer page model — RR5-FR2).

mod pdf;

pub use pdf::{PdfBackend, PDFIUM_LIB_PATH_ENV};
