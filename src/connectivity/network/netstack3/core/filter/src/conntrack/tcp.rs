// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TCP state tracking.

use core::time::Duration;

use netstack3_base::SegmentHeader;

use super::ConnectionDirection;

/// The time since the last seen packet after which an unestablished TCP
/// connection is considered expired and is eligible for garbage collection.
///
/// This is small because it's just meant to be the time between the initial SYN
/// and response SYN/ACK packet.
const CONNECTION_EXPIRY_TIME_TCP_UNESTABLISHED: Duration = Duration::from_secs(30);

/// The time since the last seen packet after which an established TCP
/// connection is considered expired and is eligible for garbage collection.
///
/// Until we have TCP tracking, this is a large value to ensure that connections
/// that are still valid aren't cleaned up prematurely.
const CONNECTION_EXPIRY_TIME_TCP_ESTABLISHED: Duration = Duration::from_secs(6 * 60 * 60);

/// A struct that completely encapsulates tracking a bidirectional TCP
/// connection.
#[derive(Debug, Clone)]
pub(crate) struct Connection {
    established: bool,
}

impl Connection {
    pub fn new(_segment: &SegmentHeader, _payload_len: usize) -> Option<Self> {
        Some(Self { established: false })
    }

    pub fn expiry_duration(&self) -> Duration {
        if self.is_established() {
            CONNECTION_EXPIRY_TIME_TCP_ESTABLISHED
        } else {
            CONNECTION_EXPIRY_TIME_TCP_UNESTABLISHED
        }
    }

    pub fn is_established(&self) -> bool {
        self.established
    }

    pub fn update(
        &mut self,
        _segment: &SegmentHeader,
        _payload_len: usize,
        dir: ConnectionDirection,
    ) {
        match dir {
            ConnectionDirection::Original => {}
            ConnectionDirection::Reply => self.established = true,
        }
    }
}
