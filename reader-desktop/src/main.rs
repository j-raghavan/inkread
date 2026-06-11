//! `reader-desktop` — the host dev harness (RR1-FR1, RR1-AC3).
//!
//! Drives the full M0 core round-trip with no device and no Android SDK: open a PDF (if a
//! host libpdfium and a file are provided) or a synthetic stub document, render each page
//! into a host buffer, turn N pages, and print the [`RefreshCommand`] stream the policy
//! emits via the [`MockDeviceRecorder`]. This is the host end-to-end proof of IR-2 (the
//! policy is identical to the device's) without hardware.
//!
//! Usage:
//! ```text
//!   reader-desktop [PATH.pdf] [TURNS] [--profile baseline|full|mock]
//! ```
//! With no PATH it runs a 12-page stub document. Set `PDFIUM_DYNAMIC_LIB_PATH` to render a
//! real PDF on the host.

use std::process::ExitCode;

use device_eink::{DeviceCapabilities, MockDeviceRecorder, RefreshCommand, RefreshIntent};
use reader_core::document::{Document, DocumentMetadata};
use reader_core::error::{CoreError, CoreResult};
use reader_core::render::{PixelBuffer, Viewport};
use reader_core::session::{Gesture, ReaderSession};

/// A synthetic document for when no PDF/pdfium is available.
struct StubDoc {
    pages: usize,
}
impl Document for StubDoc {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: Some("stub document".into()),
            author: Some("reader-desktop".into()),
        }
    }
    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if index >= self.pages {
            return Err(CoreError::PageOutOfRange {
                requested: index,
                available: self.pages,
            });
        }
        buf.fill_white();
        Ok(())
    }
}

fn profile_for(name: &str) -> DeviceCapabilities {
    match name {
        "baseline" => DeviceCapabilities::supernote_baseline(),
        "mock" => DeviceCapabilities::desktop_mock(),
        _ => DeviceCapabilities::supernote_full(),
    }
}

fn describe(cmd: &RefreshCommand) -> String {
    match cmd {
        RefreshCommand::Update {
            rect,
            intent,
            dither,
        } => {
            let i = match intent {
                RefreshIntent::Full => "Full",
                RefreshIntent::Partial => "Partial",
                RefreshIntent::Ui => "Ui",
                RefreshIntent::Fast => "Fast",
                RefreshIntent::FlashUi => "FlashUi",
                RefreshIntent::FlashPartial => "FlashPartial",
            };
            format!(
                "Update{{ {}@({},{}) {}x{}{} }}",
                i,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                if *dither { " +dither" } else { "" }
            )
        }
        RefreshCommand::WaitForLast => "WaitForLast".into(),
        RefreshCommand::EnterFastMode => "EnterFastMode".into(),
        RefreshCommand::LeaveFastMode => "LeaveFastMode".into(),
    }
}

fn run() -> CoreResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut path: Option<String> = None;
    let mut turns: usize = 8;
    let mut profile = "full".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    profile = p.clone();
                }
            }
            s if s.ends_with(".pdf") => path = Some(s.to_string()),
            s if s.parse::<usize>().is_ok() => turns = s.parse().unwrap(),
            other => eprintln!("ignoring unknown arg: {other}"),
        }
        i += 1;
    }

    let caps = profile_for(&profile);
    let viewport = Viewport::new(1404, 1872, 226);

    let mut session = match &path {
        Some(p) => {
            let bytes = std::fs::read(p)
                .map_err(|e| CoreError::InvalidArgument(format!("read {p}: {e}")))?;
            ReaderSession::open_pdf(bytes, caps, viewport)?
        }
        None => ReaderSession::with_document(Box::new(StubDoc { pages: 12 }), caps, viewport),
    };

    let md = session.metadata();
    println!("inkread reader-desktop — host policy harness");
    println!("profile      : {profile}  (eink_full={})", caps.eink_full);
    println!(
        "document     : {} ({} pages){}",
        path.as_deref().unwrap_or("<stub>"),
        session.page_count(),
        md.title
            .map(|t| format!("  title=\"{t}\""))
            .unwrap_or_default()
    );
    println!("turns        : {turns}");
    println!("---");

    // Render the first page into a host buffer (exercises the render path end-to-end).
    let mut pixels = vec![0u8; viewport.byte_len()];
    {
        let mut pb = PixelBuffer::for_viewport(&mut pixels, viewport)?;
        match session.render_current(&mut pb) {
            Ok(()) => println!("render page 0 : ok ({} bytes)", viewport.byte_len()),
            Err(e) => println!("render page 0 : skipped ({e})"),
        }
    }
    println!("---");

    // Turn N pages, recording the policy's command stream.
    let mut rec = MockDeviceRecorder::with_profile(caps);
    for n in 1..=turns {
        let cmds = session.on_gesture(Gesture::NextPage);
        for c in &cmds {
            println!("turn {n:>2} -> {}", describe(c));
        }
        rec.execute_all(cmds);
    }
    println!("---");
    println!(
        "total commands recorded: {} (now on page {})",
        rec.recorded().len(),
        session.current_page()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
