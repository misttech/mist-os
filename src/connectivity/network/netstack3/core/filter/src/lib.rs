// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Packet filtering framework.

#![no_std]
#![deny(missing_docs)]

extern crate fakealloc as alloc;

mod api;
mod context;
mod logic;
mod matchers;
mod packets;
mod state;

pub use api::FilterApi;
pub use context::{FilterBindingsTypes, FilterContext, FilterIpContext};
pub use logic::{FilterHandler, FilterImpl, Verdict};
pub use matchers::{
    AddressMatcher, AddressMatcherType, InterfaceMatcher, InterfaceProperties, PacketMatcher,
    PortMatcher, TransportProtocolMatcher,
};
pub use packets::{IpPacket, TransportPacket};
pub use state::{
    validation::{ValidState, ValidationError, ValidationInfo},
    Action, Hook, IpRoutines, NatRoutines, Routine, Rule, State, UninstalledRoutine,
};
