// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;
use crate::Platform;
use std::sync::Mutex;
use std::task::{Context, Poll};

pub(crate) struct Nat64Instance {
    nat64_prefix_req_sender: Mutex<fmpsc::UnboundedSender<ot::NetifIndex>>,
}

impl Nat64Instance {
    pub fn new(nat64_prefix_req_sender: fmpsc::UnboundedSender<u32>) -> Nat64Instance {
        Nat64Instance { nat64_prefix_req_sender: Mutex::new(nat64_prefix_req_sender) }
    }
}

impl PlatformBacking {
    fn on_nat64_prefix_request(&self, infra_if_idx: ot::NetifIndex) {
        #[no_mangle]
        unsafe extern "C" fn otPlatInfraIfDiscoverNat64Prefix(infra_if_idx: u32) {
            PlatformBacking::on_nat64_prefix_request(
                // SAFETY: Must only be called from OpenThread thread
                PlatformBacking::as_ref(),
                infra_if_idx,
            )
        }

        self.nat64
            .nat64_prefix_req_sender
            .lock()
            .expect("nat64 req sender")
            .unbounded_send(infra_if_idx)
            .expect("on_net64_prefix_request");
    }
}

impl Platform {
    pub fn process_poll_nat64(&mut self, instance: &ot::Instance, cx: &mut Context<'_>) {
        while let Poll::Ready(Some(infra_if_idx)) =
            self.nat64_prefix_req_receiver.poll_next_unpin(cx)
        {
            let ip6_prefix = openthread_sys::otIp6Prefix::default();
            unsafe {
                openthread_sys::otPlatInfraIfDiscoverNat64PrefixDone(
                    instance.as_ot_ptr(),
                    infra_if_idx,
                    &ip6_prefix,
                )
            }
        }
    }
}
