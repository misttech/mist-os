// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

extern crate netstack3_core_loom as netstack3_core;

use std::num::NonZeroU16;

use loom::sync::Arc;
use net_types::{ethernet::Mac, UnicastAddr};
use netstack3_core::{
    device::socket::{Protocol, TargetDevice},
    sync::Mutex,
    testutil::{FakeCtx, FakeEventDispatcherBuilder},
};
use packet::{Buf, Serializer as _};
use packet_formats::ethernet::{EtherType, EthernetFrameBuilder};

#[test]
fn packet_socket_change_device_and_protocol_atomic() {
    const DEVICE_MAC: Mac = net_declare::net_mac!("22:33:44:55:66:77");
    const SRC_MAC: Mac = net_declare::net_mac!("88:88:88:88:88:88");

    let make_ethernet_frame = |ethertype| {
        Buf::new(vec![1; 10], ..)
            .encapsulate(EthernetFrameBuilder::new(SRC_MAC, DEVICE_MAC, ethertype, 0))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
    };
    let first_proto: NonZeroU16 = NonZeroU16::new(EtherType::Ipv4.into()).unwrap();
    let second_proto: NonZeroU16 = NonZeroU16::new(EtherType::Ipv6.into()).unwrap();

    loom::model(move || {
        let mut builder = FakeEventDispatcherBuilder::default();
        let dev_indexes =
            [(); 2].map(|()| builder.add_device(UnicastAddr::new(DEVICE_MAC).unwrap()));
        let (FakeCtx { sync_ctx, non_sync_ctx }, indexes_to_device_ids) = builder.build();

        let sync_ctx = Arc::new(sync_ctx);
        let devs = dev_indexes.map(|i| indexes_to_device_ids[i].clone());
        drop(indexes_to_device_ids);

        let socket = netstack3_core::device::socket::create(&*sync_ctx, Mutex::new(Vec::new()));
        netstack3_core::device::socket::set_device_and_protocol(
            &*sync_ctx,
            &socket,
            TargetDevice::SpecificDevice(&devs[0].clone().into()),
            Protocol::Specific(first_proto),
        );

        let thread_vars = (sync_ctx.clone(), non_sync_ctx.clone(), devs.clone());
        let deliver = loom::thread::spawn(move || {
            let (sync_ctx, mut non_sync_ctx, devs) = thread_vars;
            for (device, ethertype) in [
                (&devs[0], first_proto.get().into()),
                (&devs[0], second_proto.get().into()),
                (&devs[1], first_proto.get().into()),
                (&devs[1], second_proto.get().into()),
            ] {
                netstack3_core::device::receive_frame(
                    &*sync_ctx,
                    &mut non_sync_ctx,
                    &device,
                    make_ethernet_frame(ethertype),
                );
            }
        });

        let thread_vars = (sync_ctx.clone(), devs[1].clone(), socket.clone());

        let change_device = loom::thread::spawn(move || {
            let (sync_ctx, dev, socket) = thread_vars;
            netstack3_core::device::socket::set_device_and_protocol(
                &*sync_ctx,
                &socket,
                TargetDevice::SpecificDevice(&dev.clone().into()),
                Protocol::Specific(second_proto),
            );
        });

        deliver.join().unwrap();
        change_device.join().unwrap();

        // These are all the matching frames. Depending on how the threads
        // interleave, one or both of them should be delivered.
        let matched_frames = [
            (
                devs[0].downgrade().into(),
                make_ethernet_frame(first_proto.get().into()).into_inner(),
            ),
            (
                devs[1].downgrade().into(),
                make_ethernet_frame(second_proto.get().into()).into_inner(),
            ),
        ];
        let received_frames = socket.socket_state().lock();
        match &received_frames[..] {
            [one] => {
                assert!(one == &matched_frames[0] || one == &matched_frames[1]);
            }
            [one, two] => {
                assert_eq!(one, &matched_frames[0]);
                assert_eq!(two, &matched_frames[1]);
            }
            other => panic!("unexpected received frames {other:?}"),
        }
    });
}
