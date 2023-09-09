// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    util::{TryFromFidlWithContext as _, TryIntoCore as _},
    Ctx,
};

use fidl_fuchsia_net as fidl_net;
use fidl_fuchsia_net_stack::{
    self as fidl_net_stack, ForwardingEntry, StackRequest, StackRequestStream,
};
use futures::{TryFutureExt as _, TryStreamExt as _};
use net_types::ip::{Ip, Ipv4, Ipv6};
use netstack3_core::ip::types::{AddableEntry, AddableEntryEither};
use tracing::{debug, error};

pub(crate) struct StackFidlWorker {
    netstack: crate::bindings::Netstack,
}

impl StackFidlWorker {
    pub(crate) async fn serve(
        netstack: crate::bindings::Netstack,
        stream: StackRequestStream,
    ) -> Result<(), fidl::Error> {
        stream
            .try_fold(Self { netstack }, |mut worker, req| async {
                match req {
                    StackRequest::AddForwardingEntry { entry, responder } => {
                        responder.send(
                            worker.fidl_add_forwarding_entry(entry).await
                        ).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                    }
                    StackRequest::DelForwardingEntry {
                        entry:
                            fidl_net_stack::ForwardingEntry {
                                subnet,
                                device_id: _,
                                next_hop: _,
                                metric: _,
                            },
                        responder,
                    } => {
                        responder.send(
                            worker.fidl_del_forwarding_entry(subnet).await
                        ).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                    }
                    StackRequest::SetInterfaceIpForwardingDeprecated {
                        id: _,
                        ip_version: _,
                        enabled: _,
                        responder,
                    } => {
                        // TODO(https://fxbug.dev/76987): Support configuring
                        // per-NIC forwarding.
                        responder.send(Err(fidl_net_stack::Error::NotSupported)).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                    }
                    StackRequest::GetDnsServerWatcher { watcher, control_handle: _ } => {
                        let () = watcher
                            .close_with_epitaph(fuchsia_zircon::Status::NOT_SUPPORTED)
                            .unwrap_or_else(|e| {
                                debug!("failed to close DNS server watcher {:?}", e)
                            });
                    }
                    StackRequest::SetDhcpClientEnabled { responder, id: _, enable } => {
                        // TODO(https://fxbug.dev/81593): Remove this once
                        // DHCPv4 client is implemented out-of-stack.
                        if enable {
                            error!("TODO(https://fxbug.dev/111066): Support starting DHCP client");
                        }
                        responder.send(Ok(())).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                    }
                    StackRequest::BridgeInterfaces{ interfaces: _, bridge, control_handle: _ } => {
                        error!("TODO(https://fxbug.dev/86661): Support bridging in NS3, probably via a new API");
                        bridge.close_with_epitaph(fuchsia_zircon::Status::NOT_SUPPORTED)
                        .unwrap_or_else(|e| {
                            debug!("failed to close bridge control {:?}", e)
                        });
                    }
                }
                Ok(worker)
            })
            .map_ok(|Self { netstack: _ }| ())
            .await
    }

    async fn fidl_add_forwarding_entry(
        &mut self,
        entry: ForwardingEntry,
    ) -> Result<(), fidl_net_stack::Error> {
        let non_sync_ctx = self.netstack.ctx.non_sync_ctx();
        let entry = match AddableEntryEither::try_from_fidl_with_ctx(non_sync_ctx, entry) {
            Ok(entry) => entry,
            Err(e) => return Err(e.into()),
        };

        type DeviceId = netstack3_core::device::DeviceId<crate::bindings::BindingsNonSyncCtxImpl>;
        fn try_to_storable_entry<I: Ip>(
            ctx: &mut Ctx,
            entry: AddableEntry<I::Addr, Option<DeviceId>>,
        ) -> Option<AddableEntry<I::Addr, DeviceId>> {
            let AddableEntry { subnet, device, gateway, metric } = entry;
            let sync_ctx = ctx.sync_ctx();
            let (device, gateway) = match (device, gateway) {
                (Some(device), gateway) => (device, gateway),
                (None, gateway) => {
                    let gateway = gateway?;
                    let device =
                        netstack3_core::select_device_for_gateway(sync_ctx, gateway.into())?;
                    (device, Some(gateway))
                }
            };
            Some(AddableEntry { subnet, device, gateway, metric })
        }

        let entry = match entry {
            AddableEntryEither::V4(entry) => {
                try_to_storable_entry::<Ipv4>(&mut self.netstack.ctx, entry)
                    .ok_or(fidl_net_stack::Error::BadState)?
                    .map_device_id(|d| d.downgrade())
                    .into()
            }
            AddableEntryEither::V6(entry) => {
                try_to_storable_entry::<Ipv6>(&mut self.netstack.ctx, entry)
                    .ok_or(fidl_net_stack::Error::BadState)?
                    .map_device_id(|d| d.downgrade())
                    .into()
            }
        };

        self.netstack
            .ctx
            .non_sync_ctx()
            .apply_route_change_either(crate::bindings::routes::ChangeEither::add(entry))
            .await
            .map_err(|err| match err {
                crate::bindings::routes::Error::AlreadyExists => {
                    fidl_net_stack::Error::AlreadyExists
                }
                crate::bindings::routes::Error::NotFound => fidl_net_stack::Error::NotFound,
                crate::bindings::routes::Error::DeviceRemoved => fidl_net_stack::Error::InvalidArgs,
                crate::bindings::routes::Error::ShuttingDown => panic!(
                    "can't apply route change because route change runner has been shut down"
                ),
            })
    }

    async fn fidl_del_forwarding_entry(
        &mut self,
        subnet: fidl_net::Subnet,
    ) -> Result<(), fidl_net_stack::Error> {
        let non_sync_ctx = self.netstack.ctx.non_sync_ctx_mut();
        if let Ok(subnet) = subnet.try_into_core() {
            non_sync_ctx
                .apply_route_change_either(match subnet {
                    net_types::ip::SubnetEither::V4(subnet) => {
                        crate::bindings::routes::Change::RemoveToSubnet(subnet).into()
                    }
                    net_types::ip::SubnetEither::V6(subnet) => {
                        crate::bindings::routes::Change::RemoveToSubnet(subnet).into()
                    }
                })
                .await
                .map_err(|err| match err {
                    crate::bindings::routes::Error::AlreadyExists => {
                        fidl_net_stack::Error::AlreadyExists
                    }
                    crate::bindings::routes::Error::NotFound => fidl_net_stack::Error::NotFound,
                    crate::bindings::routes::Error::DeviceRemoved => {
                        fidl_net_stack::Error::InvalidArgs
                    }
                    crate::bindings::routes::Error::ShuttingDown => panic!(
                        "can't apply route change because route change runner has been shut down"
                    ),
                })
        } else {
            Err(fidl_net_stack::Error::InvalidArgs)
        }
    }
}
