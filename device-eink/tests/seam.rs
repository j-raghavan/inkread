//! RR2-AC3 — the substituted-stub-adapter seam proof.
//!
//! The spec AC: *"Given the interface and a stub second adapter in a host test, when it is
//! substituted, then the core compiles and runs unchanged — proving the seam (no vendor name
//! in `reader-core`)."*
//!
//! This integration test uses ONLY the public `device-eink` surface. It defines a trivial
//! stub `RefreshPolicy` (the "core/policy" producing a `Vec<RefreshCommand>`) and a trivial
//! SECOND executor (`CountingExecutor`) distinct from `MockDeviceRecorder`, then drives the
//! **same** command stream through both executors and asserts both consume it identically.
//! Because the policy is written against the `RefreshCommand`/`RefreshPolicy` contract —
//! never against a concrete executor or a vendor — swapping the executor requires no change
//! to the policy: that is the seam.

use device_eink::{
    DeviceCapabilities, MockDeviceRecorder, Rect, RefreshCommand, RefreshIntent, RefreshPolicy,
};

/// A minimal policy standing in for the engine's: a page turn emits a Partial, with every
/// `interval`-th turn promoted to a flashing Full (the RR3-FR3 shape, kept tiny for the seam
/// proof so the test depends on nothing in `reader-core`).
struct StubPolicy {
    screen: Rect,
    partial_count: u32,
    interval: u32,
}

impl StubPolicy {
    fn new(screen: Rect, interval: u32) -> Self {
        Self {
            screen,
            partial_count: 0,
            interval: interval.max(1),
        }
    }
}

impl RefreshPolicy for StubPolicy {
    fn on_page_turn(&mut self, page_rect: Rect) -> Vec<RefreshCommand> {
        self.partial_count += 1;
        if self.partial_count >= self.interval {
            self.partial_count = 0;
            vec![
                RefreshCommand::WaitForLast,
                RefreshCommand::Update {
                    rect: page_rect,
                    intent: RefreshIntent::Full,
                    dither: false,
                },
            ]
        } else {
            vec![RefreshCommand::Update {
                rect: page_rect,
                intent: RefreshIntent::Partial,
                dither: false,
            }]
        }
    }

    fn on_scroll_start(&mut self) -> Vec<RefreshCommand> {
        Vec::new()
    }
    fn on_scroll_update(&mut self, _dirty: Rect) -> Vec<RefreshCommand> {
        Vec::new()
    }
    fn on_scroll_end(&mut self, settle: Rect) -> Vec<RefreshCommand> {
        vec![RefreshCommand::Update {
            rect: settle,
            intent: RefreshIntent::Partial,
            dither: false,
        }]
    }
    fn on_menu(&mut self, _open: bool, _region: Rect) -> Vec<RefreshCommand> {
        vec![RefreshCommand::Update {
            rect: self.screen,
            intent: RefreshIntent::Ui,
            dither: false,
        }]
    }
    fn on_night_mode(&mut self, _on: bool) -> Vec<RefreshCommand> {
        vec![RefreshCommand::Update {
            rect: self.screen,
            intent: RefreshIntent::Full,
            dither: false,
        }]
    }
}

/// A SECOND executor, distinct from `MockDeviceRecorder` — it does not record the stream, it
/// only counts commands by kind. A real adapter (Supernote/Boox) is just another `execute`.
#[derive(Default)]
struct CountingExecutor {
    updates: usize,
    barriers: usize,
    fast_mode_toggles: usize,
}

impl CountingExecutor {
    fn execute(&mut self, command: RefreshCommand) {
        match command {
            RefreshCommand::Update { .. } => self.updates += 1,
            RefreshCommand::WaitForLast => self.barriers += 1,
            RefreshCommand::EnterFastMode | RefreshCommand::LeaveFastMode => {
                self.fast_mode_toggles += 1
            }
        }
    }

    fn execute_all(&mut self, commands: impl IntoIterator<Item = RefreshCommand>) {
        for c in commands {
            self.execute(c);
        }
    }
}

/// Drive a `RefreshPolicy` (generic — accepts ANY policy, no vendor knowledge) for `turns`
/// page turns, returning the concatenated command stream. This is the "core" code path: it
/// is written against the trait, so it is unchanged regardless of which executor runs the
/// result.
fn drive<P: RefreshPolicy>(policy: &mut P, page: Rect, turns: usize) -> Vec<RefreshCommand> {
    let mut stream = Vec::new();
    for _ in 0..turns {
        stream.extend(policy.on_page_turn(page));
    }
    stream
}

#[test]
fn substituted_stub_executor_consumes_the_same_stream() {
    let screen = Rect::full(1404, 1872);
    let page = Rect::full(1404, 1872);

    // The policy is driven ONCE; the resulting stream is fed to two different executors.
    let mut policy = StubPolicy::new(screen, 6);
    let stream = drive(&mut policy, page, 6); // 5 Partial + WaitForLast + Full = 7 commands
    assert_eq!(stream.len(), 7);

    // Executor A: the MockDeviceRecorder (records).
    let mut recorder = MockDeviceRecorder::with_profile(DeviceCapabilities::desktop_mock());
    recorder.execute_all(stream.clone());

    // Executor B: a DISTINCT second adapter (counts) — substituted with zero policy change.
    let mut counting = CountingExecutor::default();
    counting.execute_all(stream.clone());

    // Both executors observed the identical stream — the seam holds (RR2-AC3).
    assert_eq!(recorder.recorded(), stream.as_slice());
    assert_eq!(counting.updates, 6); // 5 Partial + 1 Full
    assert_eq!(counting.barriers, 1); // the WaitForLast before the flash
    assert_eq!(counting.fast_mode_toggles, 0);
}

#[test]
fn the_same_driver_works_for_any_policy_unchanged() {
    // The generic `drive` is the "core" — it compiles and runs unchanged across policies,
    // proving it is written to the trait, not to a concrete type (no vendor name reachable).
    let screen = Rect::full(800, 600);
    let mut a = StubPolicy::new(screen, 2);
    let mut b = StubPolicy::new(screen, 3);

    let mut exec = CountingExecutor::default();
    exec.execute_all(drive(&mut a, screen, 2)); // interval 2: Partial, then WaitForLast+Full
    exec.execute_all(drive(&mut b, screen, 3)); // interval 3: Partial,Partial, WaitForLast+Full

    // a: 1 Partial + (WaitForLast+Full) = 2 updates, 1 barrier
    // b: 2 Partial + (WaitForLast+Full) = 3 updates, 1 barrier
    assert_eq!(exec.updates, 5);
    assert_eq!(exec.barriers, 2);
}
