// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct HandleRights(u32);

bitflags! {
    impl HandleRights: u32 {
        const DUPLICATE = 1 << 0;
        const TRANSFER = 1 << 1;
        const READ = 1 << 2;
        const WRITE = 1 << 3;
        const EXECUTE = 1 << 4;
        const MAP = 1 << 5;
        const GET_PROPERTY = 1 << 6;
        const SET_PROPERTY = 1 << 7;
        const ENUMERATE = 1 << 8;
        const DESTROY = 1 << 9;
        const SET_POLICY = 1 << 10;
        const GET_POLICY = 1 << 11;
        const SIGNAL = 1 << 12;
        const SIGNAL_PEER = 1 << 13;
        const WAIT = 1 << 14;
        const INSPECT = 1 << 15;
        const MANAGE_JOB = 1 << 16;
        const MANAGE_PROCESS = 1 << 17;
        const MANAGE_THREAD = 1 << 18;
        const APPLY_PROFILE = 1 << 19;

        const SAME_RIGHTS = 1 << 31;

        const BASIC_RIGHTS = {
            Self::TRANSFER.bits()
            | Self::DUPLICATE.bits()
            | Self::WAIT.bits()
            | Self::INSPECT.bits()
        };
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandleSubtype {
    Handle,
    Bti,
    Channel,
    Clock,
    DebugLog,
    Event,
    Eventpair,
    Exception,
    Fifo,
    Guest,
    Interrupt,
    Iommu,
    Job,
    Pager,
    PciDevice,
    Pmt,
    Port,
    Process,
    Profile,
    Resource,
    Socket,
    Stream,
    SuspendToken,
    Thread,
    Time,
    Vcpu,
    Vmar,
    Vmo,
}
