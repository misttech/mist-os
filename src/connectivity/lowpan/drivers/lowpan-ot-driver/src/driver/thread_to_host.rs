// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

use futures::prelude::*;

use lowpan_driver_common::spinel::Subnet;
use tracing::{debug, error, info, trace, warn};

impl<OT, NI, BI> OtDriver<OT, NI, BI>
where
    OT: Send + ot::InstanceInterface,
    NI: NetworkInterface,
    BI: BackboneInterface,
{
    pub fn start_multicast_routing_manager(&mut self) {
        match self.backbone_if.get_nicid() {
            None => info!("Backbone interface not present, not starting multicast routing manager"),
            Some(backbone_nicid) => {
                self.multicast_routing_manager.start(backbone_nicid.into(), self.net_if.get_index())
            }
        }
    }

    pub fn on_ot_ip_receive(
        &self,
        msg: OtMessageBox<'_>,
        frame_type: fidl_fuchsia_hardware_network::FrameType,
    ) {
        // NOTE: DRIVER STATE IS ALREADY LOCKED WHEN THIS IS CALLED!
        //       Calling `lock()` on the driver state will deadlock!

        if !msg.is_link_security_enabled() {
            // TODO: Check firewall.
            return;
        }

        // Unfortunately we must render the packet out before we can pass it along.
        let packet = msg.to_vec();

        if let Err(err) =
            self.net_if.inbound_packet_to_stack(&packet, frame_type).now_or_never().transpose()
        {
            error!("Unable to send packet to netstack: {:?}", err);
        }
    }

    pub(crate) async fn on_ot_state_change(
        &self,
        flags: ot::ChangedFlags,
    ) -> Result<(), anyhow::Error> {
        debug!("OpenThread State Change: {:?}", flags);
        self.update_connectivity_state();

        // TODO(rquattle): Consider make this a little more selective, this async-condition
        //                 is a bit of a big hammer.
        if flags.intersects(
            ot::ChangedFlags::THREAD_NETWORK_NAME
                | ot::ChangedFlags::THREAD_CHANNEL
                | ot::ChangedFlags::THREAD_PANID
                | ot::ChangedFlags::THREAD_EXT_PANID
                | ot::ChangedFlags::THREAD_ROLE
                | ot::ChangedFlags::JOINER_STATE
                | ot::ChangedFlags::ACTIVE_DATASET,
        ) {
            self.driver_state_change.trigger();
        }

        if flags.intersects(
            ot::ChangedFlags::THREAD_ROLE
                | ot::ChangedFlags::THREAD_EXT_PANID
                | ot::ChangedFlags::THREAD_NETWORK_NAME
                | ot::ChangedFlags::ACTIVE_DATASET
                | ot::ChangedFlags::THREAD_PARTITION_ID
                | ot::ChangedFlags::THREAD_BACKBONE_ROUTER_STATE,
        ) {
            self.update_border_agent_service().await;
        }

        Ok(())
    }

    pub(crate) fn on_ot_ip6_address_info(&self, info: ot::Ip6AddressInfo<'_>, is_added: bool) {
        // NOTE: DRIVER STATE IS LOCKED WHEN THIS IS CALLED!
        //       Calling `lock()` on the driver state will deadlock!

        trace!("on_ot_ip6_address_info: is_added:{} {:?}", is_added, info);

        let subnet = Subnet { addr: *info.addr(), prefix_len: info.prefix_len() };
        if info.is_multicast() {
            if is_added {
                debug!("OpenThread JOINED multicast group: {:?}", info);
                if let Err(err) = self.net_if.join_mcast_group(info.addr()).ignore_already_exists()
                {
                    warn!("Unable to join multicast group `{:?}`: {:?}", subnet, err);
                }
            } else {
                debug!("OpenThread LEFT multicast group: {:?}", info);
                if let Err(err) = self.net_if.leave_mcast_group(info.addr()).ignore_not_found() {
                    warn!("Unable to leave multicast group `{:?}`: {:?}", subnet, err);
                }
            }
        } else if is_added {
            debug!("OpenThread ADDED address: {:?}", info);
            // TODO(b/235498515): If it looks like an RLOC, don't add it for the time being.
            if subnet.addr.segments()[4..7] == [0x0, 0xff, 0xfe00] {
                info!(
                    "HACK(b/235498515): Refusing to add {:?} because it looks like an RLOC",
                    subnet.addr
                );
            } else if let Err(err) =
                self.net_if.add_address_from_spinel_subnet(&subnet).ignore_already_exists()
            {
                warn!("Unable to add address `{:?}` to interface: {:?}", subnet, err);
            }
        } else {
            debug!("OpenThread REMOVED address: {:?}", info);
            if subnet.addr.segments()[4..7] == [0x0, 0xff, 0xfe00] {
                info!(
                    "HACK(b/235498515): Refusing to remove {:?} because it looks like an RLOC",
                    subnet.addr
                );
            } else if let Err(err) = self.net_if.remove_address(&subnet).ignore_not_found() {
                warn!("Unable to remove address `{:?}` from interface: {:?}", subnet, err);
            }
        }
    }

    pub(crate) fn on_ot_bbr_multicast_listener_event(
        &self,
        event: ot::BackboneRouterMulticastListenerEvent,
        address: &ot::Ip6Address,
    ) {
        // NOTE: DRIVER STATE IS LOCKED WHEN THIS IS CALLED!
        //       Calling `lock()` on the driver state will deadlock!

        match event {
            ot::BackboneRouterMulticastListenerEvent::ListenerAdded => {
                if let Err(err) = self.backbone_if.join_mcast_group(address).ignore_already_exists()
                {
                    warn!("Unable to join multicast group `{:?}`: {:?}", address, err);
                } else {
                    let future = self.multicast_routing_manager.add_forwarding_route(address);
                    futures::executor::block_on(future);
                }
            }

            ot::BackboneRouterMulticastListenerEvent::ListenerRemoved => {
                if let Err(err) = self.backbone_if.leave_mcast_group(address).ignore_not_found() {
                    warn!("Unable to leave multicast group `{:?}`: {:?}", address, err);
                }
            }
        }
    }
}
