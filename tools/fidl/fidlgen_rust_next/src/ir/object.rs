// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde_repr::Deserialize_repr;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize_repr)]
#[repr(u32)]
pub enum ObjectType {
    None = 0,
    Process = 1,
    Thread = 2,
    Vmo = 3,
    Channel = 4,
    Event = 5,
    Port = 6,
    Interrupt = 9,
    PciDevice = 11,
    Log = 12,
    Socket = 14,
    Resource = 15,
    EventPair = 16,
    Job = 17,
    Vmar = 18,
    Fifo = 19,
    Guest = 20,
    Vcpu = 21,
    Timer = 22,
    Iommu = 23,
    Bti = 24,
    Profile = 25,
    Pmt = 26,
    SuspendToken = 27,
    Pager = 28,
    Exception = 29,
    Clock = 30,
}
