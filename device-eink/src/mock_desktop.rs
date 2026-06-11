//! `mock_desktop` — a host executor that records the command stream (RR2-FR6).
//!
//! It "executes" a [`RefreshCommand`] by appending it to a log, under a configurable
//! [`DeviceCapabilities`] profile, so the policy and the full open→render→gesture
//! round-trip are testable with no device (RR2-AC2, RR3-AC1/AC3).

use crate::capabilities::DeviceCapabilities;
use crate::command::RefreshCommand;

/// Records the [`RefreshCommand`]s a policy emits, standing in for a real device adapter.
#[derive(Debug, Clone)]
pub struct MockDeviceRecorder {
    caps: DeviceCapabilities,
    recorded: Vec<RefreshCommand>,
}

impl MockDeviceRecorder {
    /// A recorder advertising `caps` (RR2-FR6).
    #[must_use]
    pub fn with_profile(caps: DeviceCapabilities) -> Self {
        Self {
            caps,
            recorded: Vec::new(),
        }
    }

    /// The capability profile this recorder advertises (the policy is built from it).
    #[must_use]
    pub fn capabilities(&self) -> DeviceCapabilities {
        self.caps
    }

    /// "Execute" one command by recording it.
    pub fn execute(&mut self, command: RefreshCommand) {
        self.recorded.push(command);
    }

    /// "Execute" a whole command stream in order.
    pub fn execute_all(&mut self, commands: impl IntoIterator<Item = RefreshCommand>) {
        self.recorded.extend(commands);
    }

    /// The recorded command stream so far.
    #[must_use]
    pub fn recorded(&self) -> &[RefreshCommand] {
        &self.recorded
    }

    /// Drop the recorded stream (keep the profile).
    pub fn clear(&mut self) {
        self.recorded.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::RefreshIntent;
    use crate::geometry::Rect;

    #[test]
    fn records_in_order_and_clears() {
        let mut rec = MockDeviceRecorder::with_profile(DeviceCapabilities::desktop_mock());
        assert!(rec.recorded().is_empty());
        assert!(rec.capabilities().eink_full);

        rec.execute(RefreshCommand::WaitForLast);
        rec.execute_all([
            RefreshCommand::EnterFastMode,
            RefreshCommand::Update {
                rect: Rect::new(0, 0, 10, 10),
                intent: RefreshIntent::Fast,
                dither: false,
            },
        ]);
        assert_eq!(rec.recorded().len(), 3);
        assert_eq!(rec.recorded()[0], RefreshCommand::WaitForLast);

        rec.clear();
        assert!(rec.recorded().is_empty());
        // Profile survives clear().
        assert!(rec.capabilities().eink_full);
    }
}
