// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ip_test_macro::ip_test;
use netstack3_base::testutil::{FakeDeviceId, TestIpExt};
use netstack3_base::CtxPair;
use packet::InnerPacketBuilder as _;
use packet_formats::ip::IpProto;

use super::*;

#[derive(Default)]
struct IpFakeCoreCtx<I: IpLayerIpExt> {
    counters: IpCounters<I>,
}

impl<I: IpLayerIpExt> CounterContext<IpCounters<I>> for IpFakeCoreCtx<I> {
    fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(&self.counters)
    }
}

type FakeCoreCtx<I> = netstack3_base::testutil::FakeCoreCtx<
    IpFakeCoreCtx<I>,
    SendIpPacketMeta<I, FakeDeviceId, SpecifiedAddr<<I as Ip>::Addr>>,
    FakeDeviceId,
>;
type FakeBindingsCtx = netstack3_base::testutil::FakeBindingsCtx<(), (), (), ()>;
type FakeCtx<I> = CtxPair<FakeCoreCtx<I>, FakeBindingsCtx>;

impl<I: IpLayerIpExt, BC> IpDeviceSendContext<I, BC> for FakeCoreCtx<I> {
    fn send_ip_frame<S>(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _destination: IpPacketDestination<I, &Self::DeviceId>,
        _body: S,
        _egress_proof: filter::ProofOfEgressCheck,
    ) -> Result<(), netstack3_base::SendFrameError<S>>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        unimplemented!()
    }
}

#[ip_test(I)]
fn no_loopback_addrs_on_the_wire<I: IpLayerIpExt + TestIpExt>() {
    let mut ctx = FakeCtx::default();
    const TTL: u8 = 1;
    let frame = [].into_serializer().encapsulate(I::PacketBuilder::new(
        I::TEST_ADDRS.local_ip.get(),
        I::LOOPBACK_ADDRESS.get(),
        TTL,
        IpProto::Udp.into(),
    ));
    let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
    let result = send_ip_frame(
        core_ctx,
        bindings_ctx,
        &FakeDeviceId,
        IpPacketDestination::Neighbor(I::TEST_ADDRS.remote_ip),
        frame,
        IpLayerPacketMetadata::default(),
    )
    .map_err(|e| e.into_err());
    assert_eq!(result, Err(IpSendFrameErrorReason::IllegalLoopbackAddress));
}
