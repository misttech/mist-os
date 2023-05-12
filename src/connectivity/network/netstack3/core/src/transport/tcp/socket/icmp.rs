// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines how TCP sockets interacts with incoming ICMP messages.

use core::num::NonZeroU16;

use net_types::SpecifiedAddr;
use packet::BufferView as _;
use packet_formats::tcp::TcpFlowAndSeqNum;

use crate::{
    ip::{
        icmp::{IcmpErrorCode, IcmpIpExt},
        IpLayerIpExt, IpTransportContext,
    },
    transport::tcp::{
        seqnum::SeqNum,
        socket::{self as tcp_socket, SocketHandler, TcpIpTransportContext},
    },
};

impl<
        I: IcmpIpExt + IpLayerIpExt,
        C: tcp_socket::NonSyncContext,
        SC: tcp_socket::SyncContext<I, C>,
    > IpTransportContext<I, C, SC> for TcpIpTransportContext
where
    I::ErrorCode: Into<IcmpErrorCode>,
{
    fn receive_icmp_error(
        sync_ctx: &mut SC,
        ctx: &mut C,
        _device: &SC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        mut original_body: &[u8],
        err: I::ErrorCode,
    ) {
        let mut buffer = &mut original_body;
        let Some(flow_and_seqnum) = buffer
            .take_obj_front::<TcpFlowAndSeqNum>() else {
            tracing::error!("received an ICMP error but its body is less than 8 bytes");
            return;
        };

        let Some(original_src_ip) = original_src_ip else { return };
        let Some(original_src_port) = NonZeroU16::new(flow_and_seqnum.src_port()) else { return };
        let Some(original_dst_port)= NonZeroU16::new(flow_and_seqnum.dst_port()) else { return };
        let original_seqnum = SeqNum::new(flow_and_seqnum.sequence_num());

        SocketHandler::on_icmp_error(
            sync_ctx,
            ctx,
            original_src_ip,
            original_dst_ip,
            original_src_port,
            original_dst_port,
            original_seqnum,
            err.into(),
        );
    }
}
