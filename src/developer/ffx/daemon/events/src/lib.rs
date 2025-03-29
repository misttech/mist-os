// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_ssh::ssh::SshError;
use ffx_target::Description;
use rcs::RcsConnection;
use std::net::SocketAddr;
use std::time::Instant;

pub trait TryIntoTargetEventInfo: Sized {
    type Error;

    /// Attempts, given a source socket address, to determine whether the
    /// received message was from a Fuchsia target, and if so, what kind. Attempts
    /// to fill in as much information as possible given the message, consuming
    /// the underlying object in the process.
    fn try_into_target_event_info(self, src: SocketAddr) -> Result<Description, Self::Error>;
}

// TODO(b/287780199): Remove once the usage in Zedboot discovery is removed/refactored.
#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub enum WireTrafficType {
    Zedboot(Description),
}

/// Encapsulates an event that occurs on the daemon.
#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    WireTraffic(WireTrafficType),
    /// A peer with the contained NodeId has been observed on the
    /// Overnet mesh.
    OvernetPeer(u64),
    /// A peer with the contained NodeId has been dropped from the
    /// Overnet mesh (there are no remaining known routes to this peer).
    OvernetPeerLost(u64),
    NewTarget(Description),
    /// An event when a target (which is not necessarily new) has been
    /// updated with more information.
    UpdatedTarget(Description),
    // TODO(awdavies): Stale target event, target shutdown event, etc.
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub enum TargetEvent {
    RcsActivated,
    Rediscovered,
    SshHostPipeErr(SshError),

    /// LHS is previous state, RHS is current state.
    ConnectionStateChanged(TargetConnectionState, TargetConnectionState),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetConnectionState {
    /// Default state: no connection, pending rediscovery.
    Disconnected,
    /// Contains the last known ping from mDNS.
    Mdns(Instant),
    /// Contains the moment the emulator was started.
    Vsock(Instant),
    /// Contains an actual connection to RCS.
    Rcs(RcsConnection),
    /// Target was manually added. A Manual target may have an associated
    /// timeout, which is a time after which the target is allowed to
    /// expire. A Manual target with no timeout will never enter the
    /// "disconnected" state, as they are not discoverable, instead
    /// they return to the "manual" state on disconnection. A Manual target
    /// with a timeout will only enter the disconnected state after that
    /// timeout has elapsed and the target has become non-responsive.
    Manual(Option<Instant>),
    /// Contains the last known interface update with a Fastboot serial number.
    Fastboot(Instant),
    /// Contains the last known interface update with a Fastboot serial number.
    Zedboot(Instant),
}

impl Default for TargetConnectionState {
    fn default() -> Self {
        TargetConnectionState::Disconnected
    }
}

impl TargetConnectionState {
    pub fn is_connected(&self) -> bool {
        match self {
            Self::Disconnected => false,
            _ => true,
        }
    }

    pub fn is_rcs(&self) -> bool {
        match self {
            TargetConnectionState::Rcs(_) => true,
            _ => false,
        }
    }

    pub fn is_manual(&self) -> bool {
        matches!(self, Self::Manual(_))
    }

    pub fn is_product(&self) -> bool {
        matches!(self, Self::Rcs(_) | Self::Mdns(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_connection_state_default() {
        assert_eq!(TargetConnectionState::default(), TargetConnectionState::Disconnected);
    }

    #[test]
    fn test_target_connection_state_impl() {
        assert!(TargetConnectionState::Manual(None).is_connected());
        assert!(TargetConnectionState::Manual(None).is_manual());
        assert!(!TargetConnectionState::Manual(None).is_product());
        assert!(!TargetConnectionState::Manual(None).is_rcs());

        assert!(TargetConnectionState::Mdns(Instant::now()).is_connected());
        assert!(TargetConnectionState::Mdns(Instant::now()).is_product());
        assert!(!TargetConnectionState::Mdns(Instant::now()).is_rcs());
        assert!(!TargetConnectionState::Mdns(Instant::now()).is_manual());

        assert!(TargetConnectionState::Zedboot(Instant::now()).is_connected());
        assert!(!TargetConnectionState::Zedboot(Instant::now()).is_product());
        assert!(!TargetConnectionState::Zedboot(Instant::now()).is_rcs());
        assert!(!TargetConnectionState::Zedboot(Instant::now()).is_manual());

        assert!(TargetConnectionState::Fastboot(Instant::now()).is_connected());
        assert!(!TargetConnectionState::Fastboot(Instant::now()).is_product());
        assert!(!TargetConnectionState::Fastboot(Instant::now()).is_rcs());
        assert!(!TargetConnectionState::Fastboot(Instant::now()).is_manual());

        assert!(!TargetConnectionState::Disconnected.is_connected());
        assert!(!TargetConnectionState::Disconnected.is_product());
        assert!(!TargetConnectionState::Disconnected.is_rcs());
        assert!(!TargetConnectionState::Disconnected.is_manual());
    }
}
