// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec::Vec;

use assert_matches::assert_matches;
use net_types::ethernet::Mac;
use net_types::ip::{Ipv6, Ipv6Addr, Subnet};
use net_types::{LinkLocalAddress as _, NonMappedAddr, Witness as _};
use packet::{Buf, InnerPacketBuilder as _, Serializer as _};
use packet_formats::icmp::ndp::options::{NdpOptionBuilder, PrefixInformation};
use packet_formats::icmp::ndp::{OptionSequenceBuilder, RouterAdvertisement};
use packet_formats::icmp::{IcmpPacketBuilder, IcmpZeroCode};
use packet_formats::ip::Ipv6Proto;
use packet_formats::ipv6::Ipv6PacketBuilder;
use packet_formats::utils::NonZeroDuration;

use netstack3_base::testutil::{TestAddrs, TestIpExt as _};
use netstack3_base::{FrameDestination, InstantContext as _};
use netstack3_core::device::{EthernetCreationProperties, EthernetLinkDevice};
use netstack3_core::testutil::{CtxPairExt as _, FakeCtx, DEFAULT_INTERFACE_METRIC};
use netstack3_device::testutil::IPV6_MIN_IMPLIED_MAX_FRAME_SIZE;
use netstack3_ip::device::testutil::with_assigned_ipv6_addr_subnets;
use netstack3_ip::device::{
    InnerSlaacTimerId, IpDeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
    SlaacConfigurationUpdate, TemporarySlaacAddressConfiguration, SLAAC_MIN_REGEN_ADVANCE,
};
use netstack3_ip::icmp::REQUIRED_NDP_IP_PACKET_HOP_LIMIT;
use netstack3_ip::{self as ip};

const SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("200a::/64");

fn build_slaac_ra_packet(
    src_ip: Ipv6Addr,
    dst_ip: Ipv6Addr,
    prefix: Ipv6Addr,
    prefix_length: u8,
    preferred_lifetime_secs: u32,
    valid_lifetime_secs: u32,
) -> Buf<Vec<u8>> {
    let p = PrefixInformation::new(
        prefix_length,
        false, /* on_link_flag */
        true,  /* autonomous_address_configuration_flag */
        valid_lifetime_secs,
        preferred_lifetime_secs,
        prefix,
    );
    let options = &[NdpOptionBuilder::PrefixInformation(p)];
    OptionSequenceBuilder::new(options.iter())
        .into_serializer()
        .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
            src_ip,
            dst_ip,
            IcmpZeroCode,
            RouterAdvertisement::new(0, false, false, 0, 0, 0),
        ))
        .encapsulate(Ipv6PacketBuilder::new(
            src_ip,
            dst_ip,
            REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
            Ipv6Proto::Icmpv6,
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
}

#[test]
fn integration_remove_all_addresses_on_ipv6_disable() {
    let TestAddrs { local_mac, remote_mac, local_ip: _, remote_ip: _, subnet: _ } =
        Ipv6::TEST_ADDRS;

    const ONE_HOUR: NonZeroDuration = NonZeroDuration::from_secs(60 * 60).unwrap();
    const TWO_HOURS: NonZeroDuration = NonZeroDuration::from_secs(2 * 60 * 60).unwrap();

    let mut ctx = FakeCtx::default();
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    let _: Ipv6DeviceConfigurationUpdate = ctx
        .core_api()
        .device_ip::<Ipv6>()
        .update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(true),
                    temporary_address_configuration: Some(
                        TemporarySlaacAddressConfiguration::Enabled {
                            temp_valid_lifetime: ONE_HOUR,
                            temp_preferred_lifetime: ONE_HOUR,
                            temp_idgen_retries: 0,
                        },
                    ),
                },
                ..Default::default()
            },
        )
        .unwrap();

    let set_ip_enabled = |ctx: &mut FakeCtx, enabled| {
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(enabled),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
    };
    set_ip_enabled(&mut ctx, true /* enabled */);
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

    // Generate stable and temporary SLAAC addresses.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Multicast),
        build_slaac_ra_packet(
            remote_mac.to_ipv6_link_local().addr().get(),
            Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get(),
            SUBNET.network(),
            SUBNET.prefix(),
            u32::try_from(TWO_HOURS.get().as_secs()).unwrap(),
            u32::try_from(TWO_HOURS.get().as_secs()).unwrap(),
        ),
    );

    let stable_addr_sub = ip::device::testutil::calculate_slaac_addr_sub(
        SUBNET,
        local_mac.to_eui64_with_magic(Mac::DEFAULT_EUI_MAGIC),
    );

    let addrs = with_assigned_ipv6_addr_subnets(&mut ctx.core_ctx(), &device_id, |addrs| {
        addrs.filter(|a| !a.addr().is_link_local()).collect::<Vec<_>>()
    });
    let (stable_addr_sub, temp_addr_sub) = assert_matches!(
        addrs[..],
        [a1, a2] => {
            let a1 = a1.to_unicast().add_witness::<NonMappedAddr<_>>().unwrap();
            let a2 = a2.to_unicast().add_witness::<NonMappedAddr<_>>().unwrap();

            assert_eq!(a1.subnet(), SUBNET);
            assert_eq!(a2.subnet(), SUBNET);
            assert_ne!(a1, a2);

            if a1 == stable_addr_sub {
                (a1, a2)
            } else {
                (a2, a1)
            }
        }
    );
    let now = ctx.bindings_ctx.now();
    let stable_addr_lifetime_until = now + TWO_HOURS.get();
    let temp_addr_lifetime_until = now + ONE_HOUR.get();

    // Account for the desync factor:
    //
    // Per RFC 8981 Section 3.8:
    //    MAX_DESYNC_FACTOR
    //       0.4 * TEMP_PREFERRED_LIFETIME.  Upper bound on DESYNC_FACTOR.
    //
    //       |  Rationale: Setting MAX_DESYNC_FACTOR to 0.4
    //       |  TEMP_PREFERRED_LIFETIME results in addresses that have
    //       |  statistically different lifetimes, and a maximum of three
    //       |  concurrent temporary addresses when the default values
    //       |  specified in this section are employed.
    //    DESYNC_FACTOR
    //       A random value within the range 0 - MAX_DESYNC_FACTOR.  It
    //       is computed each time a temporary address is generated, and
    //       is associated with the corresponding address.  It MUST be
    //       smaller than (TEMP_PREFERRED_LIFETIME - REGEN_ADVANCE).
    let temp_addr_preferred_until_end = now + ONE_HOUR.get();
    let temp_addr_preferred_until_start =
        temp_addr_preferred_until_end - ((ONE_HOUR.get() * 3) / 5);

    let timers =
        ip::device::testutil::collect_slaac_timers_integration(&mut ctx.core_ctx(), &device_id);
    assert_eq!(
        timers.get(&InnerSlaacTimerId::InvalidateSlaacAddress { addr: stable_addr_sub.addr() }),
        Some(&stable_addr_lifetime_until)
    );
    assert_eq!(
        timers.get(&InnerSlaacTimerId::DeprecateSlaacAddress { addr: stable_addr_sub.addr() }),
        Some(&stable_addr_lifetime_until)
    );
    assert_eq!(
        timers.get(&InnerSlaacTimerId::InvalidateSlaacAddress { addr: temp_addr_sub.addr() }),
        Some(&temp_addr_lifetime_until)
    );
    assert!(timers
        .get(&InnerSlaacTimerId::DeprecateSlaacAddress { addr: temp_addr_sub.addr() })
        .is_some_and(|time| {
            (temp_addr_preferred_until_start..temp_addr_preferred_until_end).contains(time)
        }));
    assert!(timers
        .get(&InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet: temp_addr_sub })
        .is_some_and(|time| {
            (temp_addr_preferred_until_start - SLAAC_MIN_REGEN_ADVANCE.get()
                ..temp_addr_preferred_until_end - SLAAC_MIN_REGEN_ADVANCE.get())
                .contains(time)
        }));
    // Disabling IP should remove all the SLAAC addresses.
    set_ip_enabled(&mut ctx, false /* enabled */);
    let addrs = with_assigned_ipv6_addr_subnets(&mut ctx.core_ctx(), &device_id, |addrs| {
        addrs.filter(|a| !a.addr().is_link_local()).collect::<Vec<_>>()
    });
    assert_matches!(addrs[..], []);
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
}
