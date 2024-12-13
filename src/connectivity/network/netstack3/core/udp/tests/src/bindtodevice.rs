// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroU16;

use either::Either;
use ip_test_macro::ip_test;
use net_types::ip::Subnet;
use net_types::ZonedAddr;
use netstack3_base::testutil::{set_logger_for_test, TestIpExt};
use netstack3_core::device::DeviceId;
use netstack3_core::ip::IpSockSendError;
use netstack3_core::routes::{AddableEntry, AddableMetric};
use netstack3_core::testutil::{CtxPairExt as _, FakeBindingsCtx, FakeCtxBuilder};
use netstack3_core::udp::SendToError;
use netstack3_core::IpExt;
use packet::Buf;

const LOCAL_PORT: NonZeroU16 = NonZeroU16::new(100).unwrap();

// Tests that attempting to send to a loopback address on a non loopback
// interface fails even when we have a matching route.
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn bindtodevice_send_to_loopback_addrs<I: IpExt + TestIpExt>() {
    set_logger_for_test();
    const HELLO: &'static [u8] = b"Hello";
    let (mut ctx, local_device_ids) = FakeCtxBuilder::with_addrs(I::TEST_ADDRS).build();
    let device_id: DeviceId<_> = local_device_ids[0].clone().into();
    ctx.test_api()
        .add_route(
            AddableEntry {
                subnet: Subnet::new(I::UNSPECIFIED_ADDRESS, 0).unwrap(),
                device: device_id.clone(),
                gateway: None,
                metric: AddableMetric::MetricTracksInterface,
            }
            .into(),
        )
        .unwrap();
    let mut api = ctx.core_api().udp::<I>();
    let socket = api.create();
    api.set_device(&socket, Some(&device_id)).unwrap();
    let result = api.send_to(
        &socket,
        Some(ZonedAddr::Unzoned(I::LOOPBACK_ADDRESS)),
        LOCAL_PORT.into(),
        Buf::new(HELLO.to_vec(), ..),
    );
    assert_eq!(
        result,
        Err(Either::Right(SendToError::Send(IpSockSendError::IllegalLoopbackAddress)))
    );
}
