// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Arbitrary packet generators.

use arbitrary::{Arbitrary, Result, Unstructured};
use net_types::{
    ethernet::Mac,
    ip::{IpAddress, Ipv4Addr},
};
use packet_formats::{
    ethernet::{EtherType, EthernetFrameBuilder, EthernetFrameLengthCheck},
    icmp::IcmpParseArgs,
    ipv4::Ipv4PacketBuilder,
    ipv6::Ipv6PacketBuilder,
    tcp::TcpParseArgs,
    udp::UdpParseArgs,
};
use zerocopy::FromBytes;

use crate::{zerocopy::ArbitraryFromBytes, Fuzzed};

impl<'a> Arbitrary<'a> for Fuzzed<EtherType> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(Self(u16::arbitrary(u)?.into()))
    }
}

impl<'a> Arbitrary<'a> for Fuzzed<EthernetFrameLengthCheck> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        const CHOICES: [EthernetFrameLengthCheck; 2] =
            [EthernetFrameLengthCheck::Check, EthernetFrameLengthCheck::NoCheck];
        // Define this with a match to ensure that CHOICES needs to be updated if
        // EthernetFrameLengthCheck is changed.
        u.choose(&CHOICES).map(|e| {
            Self(match e {
                EthernetFrameLengthCheck::Check => EthernetFrameLengthCheck::Check,
                EthernetFrameLengthCheck::NoCheck => EthernetFrameLengthCheck::NoCheck,
            })
        })
    }
}

impl<'a> Arbitrary<'a> for Fuzzed<EthernetFrameBuilder> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(Self(EthernetFrameBuilder::new(
            Mac::arbitrary_from_bytes(u)?,
            Mac::arbitrary_from_bytes(u)?,
            Fuzzed::<EtherType>::arbitrary(u)?.into(),
            u8::arbitrary(u)?.into(),
        )))
    }
}

impl<'a> Arbitrary<'a> for Fuzzed<Ipv4PacketBuilder> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let src = Ipv4Addr::arbitrary_from_bytes(u)?;
        let dst = Ipv4Addr::arbitrary_from_bytes(u)?;
        let ttl = u.arbitrary()?;
        let proto = u8::arbitrary(u)?.into();

        let mut builder = Ipv4PacketBuilder::new(src, dst, ttl, proto);
        builder.dscp(u.int_in_range(0..=(1 << 6 - 1))?);
        builder.ecn(u.int_in_range(0..=3)?);
        builder.df_flag(u.arbitrary()?);
        builder.mf_flag(u.arbitrary()?);
        builder.fragment_offset(u.int_in_range(0..=(1 << 13) - 1)?);

        Ok(Self(builder))
    }
}

impl<'a> Arbitrary<'a> for Fuzzed<Ipv6PacketBuilder> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let src = Ipv4Addr::arbitrary_from_bytes(u)?;
        let dst = Ipv4Addr::arbitrary_from_bytes(u)?;
        let ttl = u.arbitrary()?;
        let proto = u8::arbitrary(u)?.into();

        let mut builder = Ipv6PacketBuilder::new(src, dst, ttl, proto);
        builder.ds(u.int_in_range(0..=(1 << 6 - 1))?);
        builder.ecn(u.int_in_range(0..=3)?);
        builder.flowlabel(u.int_in_range(0..=(1 << 20 - 1))?);

        Ok(Self(builder))
    }
}

impl<'a, A: IpAddress + FromBytes> Arbitrary<'a> for Fuzzed<IcmpParseArgs<A>> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let src = A::arbitrary_from_bytes(u)?;
        let dst = A::arbitrary_from_bytes(u)?;
        Ok(Self(IcmpParseArgs::new(src, dst)))
    }
}

impl<'a, A: IpAddress + FromBytes> Arbitrary<'a> for Fuzzed<UdpParseArgs<A>> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let src = A::arbitrary_from_bytes(u)?;
        let dst = A::arbitrary_from_bytes(u)?;
        Ok(Self(UdpParseArgs::new(src, dst)))
    }
}

impl<'a, A: IpAddress + FromBytes> Arbitrary<'a> for Fuzzed<TcpParseArgs<A>> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let src = A::arbitrary_from_bytes(u)?;
        let dst = A::arbitrary_from_bytes(u)?;
        Ok(Self(TcpParseArgs::new(src, dst)))
    }
}
