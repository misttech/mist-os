// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ota::controller::SendEvent;
use ota_lib::OtaStatus;

pub struct FinalizeReinstallAction {}

impl FinalizeReinstallAction {
    pub fn run(_event_sender: Box<dyn SendEvent>, status: OtaStatus) {
        // TODO(b/260748079) Implement metrics upload and reboot here
        println!("Stub: Implement me. {:?}", status);
    }
}

#[cfg(test)]
mod test {}
