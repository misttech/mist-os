// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

use anyhow::Error;
use fuchsia_component::client::connect_to_protocol;
use openthread::ot::Ip4Cidr;
use std::cell::RefCell;

const NAT64_IP4CIDR_DEFAULT_SUBNET: [u8; 4] = [240, 16, 0, 0];
const NAT64_IP4CIDR_DEFAULT_PREFIX_LEN: u8 = 24;

#[derive(Debug)]
pub struct Nat64 {
    pub lowpan_nat_ctrl: RefCell<Option<fidl_fuchsia_net_masquerade::ControlProxy>>,
}

pub fn get_first_usable_addr_in_subnet(subnet: [u8; 4], prefix_len: u8) -> [u8; 4] {
    // Stop the lowpan-ot-driver for earier debugging
    assert!(prefix_len <= 32, "IP subnet prefix is too long");

    if prefix_len == 32 {
        return subnet;
    }

    let mask: u32 = !((1u64 << (32 - prefix_len)) - 1) as u32;

    // Mask out the subnet bits and then add one to represent the first usable address
    ((u32::from_be_bytes(subnet) & mask) + 1).to_be_bytes()
}

impl<OT, NI, BI> OtDriver<OT, NI, BI>
where
    OT: Send + ot::InstanceInterface + AsRef<ot::Instance>,
    NI: NetworkInterface,
    BI: BackboneInterface,
{
    /// Try initialize NAT64 at boot time.
    #[tracing::instrument(skip_all)]
    pub fn init_nat64(&self) {
        let driver_state = self.driver_state.lock();

        if let Some(wlan_client_nicid) = self.backbone_if.get_nicid() {
            if let Err(e) = driver_state.nat64.try_init(
                &driver_state.ot_instance,
                &self.net_if,
                wlan_client_nicid.into(),
            ) {
                warn!("NAT64 failed to initialize: {:?}", e);
            }
        }
    }
}

impl Nat64 {
    pub fn new() -> Self {
        Nat64 { lowpan_nat_ctrl: RefCell::new(None) }
    }

    pub fn try_init<OT, NI>(
        &self,
        ot_instance: &OT,
        net_if: &NI,
        wlan_client_nicid: u64,
    ) -> Result<(), Error>
    where
        OT: Send + ot::InstanceInterface,
        NI: NetworkInterface,
    {
        let lowpan_nicid: u64 = net_if.get_index();
        // Set NAT64 Ipv4 CIDR
        // TODO(b/278659448): use default one only if no NAT64 prefix is on the network.
        let ip4_cidr = Ip4Cidr::new(NAT64_IP4CIDR_DEFAULT_SUBNET, NAT64_IP4CIDR_DEFAULT_PREFIX_LEN);
        ot_instance.nat64_set_ip4_cidr(ip4_cidr)?;

        // Enable Masquerade
        let masquerade_proxy = connect_to_protocol::<fidl_fuchsia_net_masquerade::FactoryMarker>()
            .expect("error connecting to masquerade factory");
        let config = fidl_fuchsia_net_masquerade::ControlConfig {
            /// The interface carrying the network to be masqueraded.[lowpan0]
            input_interface: lowpan_nicid,
            /// The network to be masqueraded.
            src_subnet: fidl_fuchsia_net::Subnet {
                addr: fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address {
                    addr: NAT64_IP4CIDR_DEFAULT_SUBNET,
                }),
                prefix_len: NAT64_IP4CIDR_DEFAULT_PREFIX_LEN,
            },
            /// The interface through which to masquerade.[Wlan Client]
            output_interface: wlan_client_nicid,
        };

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_net_masquerade::ControlMarker>();

        // Don't handle the error here. If failed, control_proxy conversion will fail.
        let _ = masquerade_proxy.create(&config, server_end).now_or_never();

        let control_proxy = client_end.into_proxy().context("failed to convert ctrl proxy")?;

        control_proxy.set_enabled(true).now_or_never();

        self.lowpan_nat_ctrl.replace(Some(control_proxy));

        // Add Ipv4 route
        let addr = fidl_fuchsia_net::Subnet {
            addr: fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address {
                addr: get_first_usable_addr_in_subnet(
                    NAT64_IP4CIDR_DEFAULT_SUBNET,
                    NAT64_IP4CIDR_DEFAULT_PREFIX_LEN,
                ),
            }),
            prefix_len: NAT64_IP4CIDR_DEFAULT_PREFIX_LEN,
        };
        if let Err(err) = net_if.add_address(addr).ignore_already_exists() {
            warn!("Unable to add NAT64 route `{:?}` to lowpan interface: {:?}", addr, err);
        }
        info!("NAT64: masquerade configured with {:?}", config);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_get_first_usable_addr_in_subnet() {
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 32),
            [0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 31),
            [0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 28),
            [0xff, 0xff, 0xff, 0xf1]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 24),
            [0xff, 0xff, 0xff, 0x1]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 8),
            [0xff, 0x0, 0x0, 0x1]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet([0xff, 0xff, 0xff, 0xff], 0),
            [0x0, 0x0, 0x0, 0x1]
        );
        assert_eq!(
            get_first_usable_addr_in_subnet(
                NAT64_IP4CIDR_DEFAULT_SUBNET,
                NAT64_IP4CIDR_DEFAULT_PREFIX_LEN
            ),
            [240, 16, 0, 1]
        );
    }
}
