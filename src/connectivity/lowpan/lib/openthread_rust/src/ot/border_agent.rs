// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;
use num::FromPrimitive;

/// Represents the thread joiner state.
///
/// Functional equivalent of [`otsys::otJoinerState`](crate::otsys::otJoinerState).
#[derive(
    Debug,
    Copy,
    Clone,
    Eq,
    Ord,
    PartialOrd,
    PartialEq,
    num_derive::FromPrimitive,
    num_derive::ToPrimitive,
)]
pub enum BorderAgentEphemeralKeyState {
    /// Functional equivalent of [`otsys::OT_BORDER_AGENT_STATE_DISABLED`](crate::otsys::OT_BORDER_AGENT_STATE_DISABLED).
    Disabled = OT_BORDER_AGENT_STATE_DISABLED as isize,

    /// Functional equivalent of [`otsys::OT_BORDER_AGENT_STATE_STOPPED`](crate::otsys::OT_BORDER_AGENT_STATE_STOPPED).
    Stopped = OT_BORDER_AGENT_STATE_STOPPED as isize,

    /// Functional equivalent of [`otsys::OT_BORDER_AGENT_STATE_STARTED`](crate::otsys::OT_BORDER_AGENT_STATE_STARTED).
    Started = OT_BORDER_AGENT_STATE_STARTED as isize,

    /// Functional equivalent of [`otsys::OT_BORDER_AGENT_STATE_CONNECTED`](crate::otsys::OT_BORDER_AGENT_STATE_CONNECTED).
    Connected = OT_BORDER_AGENT_STATE_CONNECTED as isize,

    /// Functional equivalent of [`otsys::OT_BORDER_AGENT_STATE_ACCEPTED`](crate::otsys::OT_BORDER_AGENT_STATE_ACCEPTED).
    Accepted = OT_BORDER_AGENT_STATE_ACCEPTED as isize,
}

impl From<otBorderAgentEphemeralKeyState> for BorderAgentEphemeralKeyState {
    fn from(x: otBorderAgentEphemeralKeyState) -> Self {
        Self::from_u32(x)
            .unwrap_or_else(|| panic!("Unknown otBorderAgentEphemeralKeyState value: {x}"))
    }
}

impl From<BorderAgentEphemeralKeyState> for otBorderAgentEphemeralKeyState {
    fn from(x: BorderAgentEphemeralKeyState) -> Self {
        x as otBorderAgentEphemeralKeyState
    }
}

/// Methods from the [OpenThread "Border Agent" Module][1].
///
/// [1]: https://openthread.io/reference/group/api-border-agent
pub trait BorderAgent {
    /// Functional equivalent of
    /// [`otsys::otBorderAgentGetState`](crate::otsys::otBorderAgentGetState).
    fn border_agent_is_active(&self) -> bool;

    /// Functional equivalent of
    /// [`otsys::otBorderAgentUdpPort`](crate::otsys::otBorderAgentGetUdpPort).
    fn border_agent_get_udp_port(&self) -> u16;
}

impl<T: BorderAgent + Boxable> BorderAgent for ot::Box<T> {
    fn border_agent_is_active(&self) -> bool {
        self.as_ref().border_agent_is_active()
    }

    fn border_agent_get_udp_port(&self) -> u16 {
        self.as_ref().border_agent_get_udp_port()
    }
}

impl BorderAgent for Instance {
    fn border_agent_is_active(&self) -> bool {
        unsafe { otBorderAgentIsActive(self.as_ot_ptr()) }
    }

    fn border_agent_get_udp_port(&self) -> u16 {
        unsafe { otBorderAgentGetUdpPort(self.as_ot_ptr()) }
    }
}
