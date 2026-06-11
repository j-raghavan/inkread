//! `PdfBackend` — the M0 fixed-layout PDF [`Document`] over `pdfium-render` (RR5, Amendment 4).
//!
//! ## Single-copy render with explicit channel order (Fork 4 / Amendment 3)
//! The page is rendered **directly into the borrowed [`PixelBuffer`]**: a `PdfBitmap` wraps
//! the buffer's `&mut [u8]` via the (unsafe) `from_bytes`, and `set_reverse_byte_order(true)`
//! makes pdfium emit **RGBA** straight into it (pdfium's native order is BGRA). The buffer is
//! white-filled first (RR4-FR3); the channel order matches [`CHANNEL_ORDER`], asserted by the
//! grayscale golden test.
//!
//! ## Library binding (host vs device)
//! pdfium is a dynamic library. A process-global `Pdfium` (behind a `OnceLock`, `thread_safe`
//! feature) backs `PdfDocument<'static>` so the backend owns the open document without a
//! self-referential type. Binding order: an explicit `PDFIUM_DYNAMIC_LIB_PATH` (host tests /
//! a vendored binary) → the system library → else [`CoreError::BackendUnavailable`]. On
//! Android the loader finds `libpdfium.so` in `jniLibs/` via the system path.

use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

use crate::document::{Document, DocumentMetadata};
use crate::error::{CoreError, CoreResult};
use crate::render::PixelBuffer;

/// Env var naming an explicit libpdfium to bind (host testing / vendored binary).
pub const PDFIUM_LIB_PATH_ENV: &str = "PDFIUM_DYNAMIC_LIB_PATH";

/// The process-global pdfium binding (owns the library bindings; effectively `'static`).
static PDFIUM: OnceLock<Pdfium> = OnceLock::new();

/// A second, leaked binding to the same library, exposed as `&'static dyn ...`.
///
/// `Pdfium::new` consumes the box it is given and 0.9.1 exposes no accessor to recover it,
/// yet the single-copy render needs a `&dyn PdfiumLibraryBindings` for
/// `PdfBitmap::from_bytes` (Fork 4). Binding the same already-loaded library twice is sound
/// — each call returns a fresh bindings object over the same handle — so we keep one for
/// `Pdfium` and leak one to `'static` for `from_bytes`.
static BINDINGS: OnceLock<&'static dyn PdfiumLibraryBindings> = OnceLock::new();

/// Serializes first-time binding. pdfium's library init is not reentrant, so two threads
/// racing to bind on first use can abort; this lock makes init single-flight (the steady
/// state is lock-free via the `OnceLock::get` fast path).
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Bind pdfium once and return the process-global handle, or a typed error if no library
/// is available (the host without a vendored libpdfium — RR5-FR1).
fn pdfium() -> CoreResult<&'static Pdfium> {
    // Fast path: already initialized, no lock.
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    // Slow path: serialize so exactly one thread ever binds the library.
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    // Bind twice over the same library: one box for `Pdfium`, one leaked for `from_bytes`.
    let for_pdfium = bind_pdfium()?;
    let for_bitmap = bind_pdfium()?;
    let _ = PDFIUM.set(Pdfium::new(for_pdfium));
    let _ = BINDINGS.set(Box::leak(for_bitmap));
    Ok(PDFIUM.get().expect("pdfium set above"))
}

/// The `&'static dyn PdfiumLibraryBindings` for the single-copy bitmap wrap (Fork 4).
fn bindings() -> CoreResult<&'static dyn PdfiumLibraryBindings> {
    pdfium()?; // ensure both globals are initialized
    BINDINGS
        .get()
        .copied()
        .ok_or_else(|| CoreError::BackendUnavailable("pdfium bindings uninitialized".into()))
}

fn bind_pdfium() -> CoreResult<Box<dyn PdfiumLibraryBindings>> {
    if let Ok(path) = std::env::var(PDFIUM_LIB_PATH_ENV) {
        return Pdfium::bind_to_library(&path).map_err(|e| {
            CoreError::BackendUnavailable(format!("bind_to_library({path}) failed: {e}"))
        });
    }
    Pdfium::bind_to_system_library().map_err(|e| {
        CoreError::BackendUnavailable(format!(
            "no libpdfium: set {PDFIUM_LIB_PATH_ENV} or install a system library ({e})"
        ))
    })
}

/// Map a pdfium load error to a typed [`CoreError`] (DRM vs corrupt vs other — RR7-FR5).
fn map_load_error(e: PdfiumError) -> CoreError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
        | PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            CoreError::DrmProtected
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError)
        | PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FileError) => {
            CoreError::CorruptDocument(format!("pdfium: {e}"))
        }
        other => CoreError::RenderBackend(format!("pdfium load: {other}")),
    }
}

/// A loaded PDF, rendered directly into the shell's buffer (RR5, Amendment 4).
pub struct PdfBackend {
    document: PdfDocument<'static>,
}

impl PdfBackend {
    /// Open a PDF from in-memory bytes (the shell reads the file; the core never names a
    /// path scheme). DRM/corrupt files return a typed error, never a panic (RR7-FR5, RR21-FR3).
    pub fn open(bytes: Vec<u8>) -> CoreResult<Self> {
        let pdfium = pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(map_load_error)?;
        Ok(Self { document })
    }
}

impl Document for PdfBackend {
    fn page_count(&self) -> usize {
        self.document.pages().len() as usize
    }

    fn metadata(&self) -> DocumentMetadata {
        use pdfium_render::prelude::PdfDocumentMetadataTagType as Tag;
        let md = self.document.metadata();
        let get = |tag| md.get(tag).map(|t| t.value().to_string());
        DocumentMetadata {
            title: get(Tag::Title),
            author: get(Tag::Author),
        }
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        let available = self.page_count();
        let page_index = i32::try_from(index)
            .ok()
            .filter(|&i| (i as usize) < available)
            .ok_or(CoreError::PageOutOfRange {
                requested: index,
                available,
            })?;

        // White-fill before render so there are no alpha gaps (RR4-FR3).
        buf.fill_white();

        let width = buf.width();
        let height = buf.height();
        let w = i32::try_from(width)
            .map_err(|_| CoreError::BufferMismatch(format!("width {width} exceeds i32")))?;
        let h = i32::try_from(height)
            .map_err(|_| CoreError::BufferMismatch(format!("height {height} exceeds i32")))?;

        let page = self
            .document
            .pages()
            .get(page_index)
            .map_err(|e| CoreError::RenderBackend(format!("get page {index}: {e}")))?;

        // Amendment 3: reverse byte order so pdfium emits RGBA into our RGBA buffer.
        let config = PdfRenderConfig::new()
            .set_target_size(w, h)
            .set_format(PdfBitmapFormat::BGRA)
            .set_reverse_byte_order(true);

        // Single-copy: wrap the borrowed buffer as the pdfium bitmap target (Fork 4).
        // SAFETY: `buf.bytes_mut()` is exactly `width*height*4` bytes (PixelBuffer invariant),
        // matching the bitmap geometry; the borrow lives only for this call (Amendment 5).
        let lib = bindings()?;
        let mut bitmap =
            unsafe { PdfBitmap::from_bytes(w, h, PdfBitmapFormat::BGRA, buf.bytes_mut(), lib) }
                .map_err(|e| CoreError::RenderBackend(format!("wrap bitmap: {e}")))?;

        page.render_into_bitmap_with_config(&mut bitmap, &config)
            .map_err(|e| CoreError::RenderBackend(format!("render page {index}: {e}")))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whether a host libpdfium is reachable in this environment. The render/open tests are
    // gated on it so a CI box without the binary skips rather than fails (recorded as
    // host-binary-UNVERIFIED). They are NOT deleted — they run wherever pdfium is present.
    fn host_pdfium_available() -> bool {
        pdfium().is_ok()
    }

    #[test]
    fn missing_library_is_typed_not_panic() {
        // When neither the env path nor a system library resolves, binding yields a typed
        // BackendUnavailable error (never a panic) — RR21-FR3. If a library IS present in
        // this environment, the bind simply succeeds; either way, no panic.
        match pdfium() {
            Ok(_) => { /* a library is available here */ }
            Err(CoreError::BackendUnavailable(_)) => { /* expected on a bare host */ }
            Err(other) => panic!("unexpected error binding pdfium: {other}"),
        }
    }

    #[test]
    fn open_and_render_minimal_pdf() {
        if !host_pdfium_available() {
            eprintln!("SKIP open_and_render_minimal_pdf: host libpdfium UNVERIFIED (no binding)");
            return;
        }
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/minimal.pdf"
        ))
        .expect("fixture present");
        let doc = PdfBackend::open(bytes).expect("open fixture");
        assert_eq!(doc.page_count(), 1);

        // Render page 0 into a small RGBA buffer; assert it doesn't stay all-white
        // (something was drawn) and the channel order produced sane RGBA.
        let (w, h) = (120u32, 160u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let mut pb = PixelBuffer::from_rgba(&mut pixels, w, h).unwrap();
        doc.render_page(0, &mut pb).expect("render page 0");

        // Every pixel must be opaque (white-fill set α=255; pdfium keeps it opaque).
        assert!(pb.bytes().chunks_exact(4).all(|p| p[3] == 0xFF));
        // The fixture draws black text on white, so at least one pixel is non-white.
        let any_ink = pb
            .bytes()
            .chunks_exact(4)
            .any(|p| p[0] < 200 || p[1] < 200 || p[2] < 200);
        assert!(
            any_ink,
            "expected some rendered content, got an all-white page"
        );
    }

    #[test]
    fn render_out_of_range_is_typed_error() {
        if !host_pdfium_available() {
            eprintln!("SKIP render_out_of_range: host libpdfium UNVERIFIED");
            return;
        }
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/minimal.pdf"
        ))
        .expect("fixture present");
        let doc = PdfBackend::open(bytes).unwrap();
        let mut pixels = vec![0u8; 4 * 4 * 4];
        let mut pb = PixelBuffer::from_rgba(&mut pixels, 4, 4).unwrap();
        assert!(matches!(
            doc.render_page(99, &mut pb),
            Err(CoreError::PageOutOfRange { requested: 99, .. })
        ));
    }
}
