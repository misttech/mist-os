// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The special-purpose event loop used by the reachability monitor.
//!
//! This event loop receives events from netstack. Thsose events are used by the reachability
//! monitor to infer the connectivity state.

use {
    anyhow::{anyhow, Context as _},
    fidl_fuchsia_hardware_network as fhardware_network, fidl_fuchsia_net as fnet,
    fidl_fuchsia_net_debug as fnet_debug, fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext::{self as fnet_interfaces_ext, Update as _},
    fidl_fuchsia_net_name as fnet_name, fidl_fuchsia_net_neighbor as fnet_neighbor,
    fidl_fuchsia_net_stack as fnet_stack, fuchsia_async as fasync,
    fuchsia_inspect::health::Reporter,
    fuchsia_zircon as zx,
    futures::{future::FusedFuture, pin_mut, prelude::*, select},
    named_timer::NamedTimeoutExt,
    reachability_core::{watchdog, InterfaceView, Monitor, NeighborCache, FIDL_TIMEOUT_ID},
    reachability_handler::{ReachabilityHandler, ReachabilityState},
    std::collections::HashMap,
    tracing::{debug, error},
};

const REPORT_PERIOD: zx::Duration = zx::Duration::from_seconds(60);
const PROBE_PERIOD: zx::Duration = zx::Duration::from_seconds(60);
// Gstatic has a TTL of 300 seconds, therefore, we will perform a lookup every
// 300 seconds since we won't get any better indication of DNS function.
// TODO(fxbug.dev/121010): Dynamically query TTL based on the domain's DNS record
const DNS_PROBE_PERIOD: zx::Duration = zx::Duration::from_seconds(300);
const DNS_FIDL_TIMEOUT: zx::Duration = zx::Duration::from_seconds(90);
const FIDL_TIMEOUT: zx::Duration = zx::Duration::from_seconds(90);
const DNS_DOMAIN: &str = "www.gstatic.com";

struct SystemDispatcher;

#[async_trait::async_trait]
impl watchdog::SystemDispatcher for SystemDispatcher {
    type DeviceDiagnostics = DeviceDiagnosticsProvider;

    async fn log_debug_info(&self) -> Result<(), watchdog::Error> {
        let diagnostics =
            fuchsia_component::client::connect_to_protocol::<fnet_debug::DiagnosticsMarker>()
                .map_err(|e| {
                    error!(e = ?e, "failed to connect to protocol");
                    watchdog::Error::NotSupported
                })?;
        diagnostics
            .log_debug_info_to_syslog()
            .map_err(watchdog::Error::Fidl)
            .on_timeout_named(&FIDL_TIMEOUT_ID, FIDL_TIMEOUT, || Err(watchdog::Error::Timeout))
            .await
    }

    fn get_device_diagnostics(
        &self,
        interface: u64,
    ) -> Result<Self::DeviceDiagnostics, watchdog::Error> {
        let interfaces_debug =
            fuchsia_component::client::connect_to_protocol::<fnet_debug::InterfacesMarker>()
                .map_err(|e| {
                    error!(e = ?e, "failed to connect to protocol");
                    watchdog::Error::NotSupported
                })?;
        let (port, server_end) = fidl::endpoints::create_proxy()?;
        interfaces_debug.get_port(interface, server_end)?;

        let (diagnostics, server_end) = fidl::endpoints::create_proxy()?;
        port.get_diagnostics(server_end)?;

        Ok(DeviceDiagnosticsProvider { interface, diagnostics, port })
    }
}

struct DeviceDiagnosticsProvider {
    interface: u64,
    diagnostics: fhardware_network::DiagnosticsProxy,
    port: fhardware_network::PortProxy,
}

#[async_trait::async_trait]
impl watchdog::DeviceDiagnosticsProvider for DeviceDiagnosticsProvider {
    async fn get_counters(&self) -> Result<watchdog::DeviceCounters, watchdog::Error> {
        self.port
            .get_counters()
            .map_err(watchdog::Error::Fidl)
            .and_then(
                |fhardware_network::PortGetCountersResponse { rx_frames, tx_frames, .. }| match (
                    rx_frames, tx_frames,
                ) {
                    (Some(rx_frames), Some(tx_frames)) => {
                        futures::future::ok(watchdog::DeviceCounters { rx_frames, tx_frames })
                    }
                    (None, Some(_)) | (Some(_), None) | (None, None) => {
                        error!(iface = self.interface, "missing information from port counters");
                        futures::future::err(watchdog::Error::NotSupported)
                    }
                },
            )
            .on_timeout_named(&FIDL_TIMEOUT_ID, FIDL_TIMEOUT, || Err(watchdog::Error::Timeout))
            .await
    }

    async fn log_debug_info(&self) -> Result<(), watchdog::Error> {
        self.diagnostics
            .log_debug_info_to_syslog()
            .map_err(watchdog::Error::Fidl)
            .on_timeout_named(&FIDL_TIMEOUT_ID, FIDL_TIMEOUT, || Err(watchdog::Error::Timeout))
            .await
    }
}

type Watchdog = watchdog::Watchdog<SystemDispatcher>;

async fn dns_lookup(
    name_lookup: &fnet_name::LookupProxy,
) -> Result<fnet_name::LookupResult, anyhow::Error> {
    name_lookup
        .lookup_ip(
            DNS_DOMAIN,
            fnet_name::LookupIpOptions {
                ipv4_lookup: Some(true),
                ipv6_lookup: Some(true),
                ..fnet_name::LookupIpOptions::EMPTY
            },
        )
        .map_err(|e: fidl::Error| anyhow!("lookup_ip call failed: {:?}", e))
        .on_timeout_named(&FIDL_TIMEOUT_ID, DNS_FIDL_TIMEOUT, || {
            Err(anyhow!("lookup_ip timed out after {} seconds", DNS_FIDL_TIMEOUT.into_seconds()))
        })
        .await
        .map_err(|e: anyhow::Error| anyhow!("{:?}", e))?
        .map_err(|e: fnet_name::LookupError| anyhow!("lookup failed: {:?}", e))
}

/// The event loop.
pub struct EventLoop {
    monitor: Monitor,
    handler: ReachabilityHandler,
    watchdog: Watchdog,
    interface_properties: HashMap<u64, fnet_interfaces_ext::Properties>,
    neighbor_cache: NeighborCache,
    latest_dns_addresses: Vec<fnet::IpAddress>,
}

impl EventLoop {
    /// `new` returns an `EventLoop` instance.
    pub fn new(monitor: Monitor, handler: ReachabilityHandler) -> Self {
        fuchsia_inspect::component::health().set_starting_up();
        EventLoop {
            monitor,
            handler,
            watchdog: Watchdog::new(),
            interface_properties: Default::default(),
            neighbor_cache: Default::default(),
            latest_dns_addresses: Vec::new(),
        }
    }

    /// `run` starts the event loop.
    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
        use fuchsia_component::client::connect_to_protocol;

        let stack = connect_to_protocol::<fnet_stack::StackMarker>()
            .context("network_manager failed to connect to netstack")?;

        let if_watcher_stream = {
            let interface_state = connect_to_protocol::<fnet_interfaces::StateMarker>()
                .context("network_manager failed to connect to interface state")?;
            // TODO(https://fxbug.dev/110445): Don't register interest in
            // valid-until. Note that the event stream returned by the extension
            // crate is created from a watcher with interest in all fields.
            fnet_interfaces_ext::event_stream_from_state(&interface_state)
                .context("get interface event stream")?
                .fuse()
        };

        let neigh_watcher_stream = {
            let view = connect_to_protocol::<fnet_neighbor::ViewMarker>()
                .context("failed to connect to neighbor view")?;
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fnet_neighbor::EntryIteratorMarker>()
                    .context("failed to create EntryIterator proxy")?;
            let () = view
                .open_entry_iterator(server_end, fnet_neighbor::EntryIteratorOptions::EMPTY)
                .context("failed to open EntryIterator")?;
            futures::stream::try_unfold(proxy, |proxy| {
                proxy.get_next().map_ok(|e| {
                    Some((
                        futures::stream::iter(e.into_iter().map(Result::<_, fidl::Error>::Ok)),
                        proxy,
                    ))
                })
            })
            .try_flatten()
            .fuse()
        };

        let name_lookup = connect_to_protocol::<fnet_name::LookupMarker>()
            .context("failed to connect to Lookup")?;
        let dns_lookup_fut = dns_lookup(&name_lookup).fuse();

        debug!("starting event loop");
        let mut probe_futures = futures::stream::FuturesUnordered::new();
        let report_stream = fasync::Interval::new(REPORT_PERIOD).fuse();
        let dns_interval_timer = fasync::Interval::new(DNS_PROBE_PERIOD).fuse();

        pin_mut!(
            if_watcher_stream,
            report_stream,
            neigh_watcher_stream,
            dns_lookup_fut,
            dns_interval_timer
        );

        fuchsia_inspect::component::health().set_ok();

        loop {
            select! {
                if_watcher_res = if_watcher_stream.try_next() => {
                    match if_watcher_res {
                        Ok(Some(event)) => {
                            let discovered_id = self
                                .handle_interface_watcher_event(&stack, event)
                                .await
                                .context("failed to handle interface watcher event")?;
                            if let Some(id) = discovered_id {
                                probe_futures
                                    .push(fasync::Interval::new(PROBE_PERIOD)
                                    .map(move |()| id)
                                    .into_future());
                            }
                        }
                        Ok(None) => {
                            return Err(anyhow!("interface watcher stream unexpectedly ended"));
                        }
                        Err(e) => return Err(anyhow!("interface watcher stream error: {}", e)),
                    }
                },
                neigh_res = neigh_watcher_stream.try_next() => {
                    let event = neigh_res.context("neighbor stream error")?
                                         .context("neighbor stream ended")?;
                    self.neighbor_cache.process_neighbor_event(event);
                },
                report = report_stream.next() => {
                    let () = report.context("periodic timer for reporting unexpectedly ended")?;
                    let () = self.monitor.report_state();
                },
                probe = probe_futures.next() => {
                    match probe {
                        Some((Some(id), stream)) => {
                            if let Some(properties) = self.interface_properties.get(&id) {
                                Self::compute_state(
                                    &mut self.monitor,
                                    &mut self.watchdog,
                                    &stack, properties,
                                    &self.neighbor_cache
                                ).await;
                                let () = probe_futures.push(stream.into_future());
                                self.handler.set_state(
                                    ReachabilityState {
                                        internet_available: self.monitor.state().system_has_internet()
                                    }
                                ).await;
                            }
                        }
                        Some((None, _)) => {
                            return Err(anyhow!(
                                "periodic timer for probing reachability unexpectedly ended"
                            ));
                        }
                        // None is returned when there are no periodic timers in the
                        // FuturesUnordered collection.
                        None => {}
                    }
                },
                // The interval timer is longer than the lookup future timeout,
                // so the future will be terminated by the time the next
                // interval is reached.
                () = dns_interval_timer.select_next_some() => {
                    debug_assert!(dns_lookup_fut.is_terminated());
                    dns_lookup_fut.set(dns_lookup(&name_lookup).fuse());
                },
                // If the DNS lookup errors or if there are no IpAddresses returned, DNS is
                // inactive. In the future, another condition of DNS inactivity will be if the
                // returned list of IpAddresses do not fall into the expected IP ranges.
                dns_res = dns_lookup_fut => {
                    match dns_res {
                        Ok(lookup_result) => {
                            let addresses = match lookup_result.addresses {
                                Some(addrs) => addrs,
                                None => Vec::new(),
                            };

                            if addresses.len() == 0 {
                                debug!("DNS lookup result unexpectedly had 0 addresses");
                            }

                            self.latest_dns_addresses = addresses;
                        }
                        Err(e) => {
                            debug!("lookup_ip error: {:?}", e);
                            self.latest_dns_addresses = Vec::new();
                        }
                    }
                }
            }
        }
    }

    async fn handle_interface_watcher_event(
        &mut self,
        stack: &fnet_stack::StackProxy,
        event: fnet_interfaces::Event,
    ) -> Result<Option<u64>, anyhow::Error> {
        match self
            .interface_properties
            .update(event)
            .context("failed to update interface properties map with watcher event")?
        {
            fnet_interfaces_ext::UpdateResult::Added(properties)
            | fnet_interfaces_ext::UpdateResult::Existing(properties) => {
                let id = properties.id;
                debug!("setting timer for interface {}", id);
                Self::compute_state(
                    &mut self.monitor,
                    &mut self.watchdog,
                    &stack,
                    properties,
                    &self.neighbor_cache,
                )
                .await;
                return Ok(Some(id));
            }
            fnet_interfaces_ext::UpdateResult::Changed {
                previous:
                    fnet_interfaces::Properties {
                        online,
                        addresses,
                        has_default_ipv4_route,
                        has_default_ipv6_route,
                        ..
                    },
                current:
                    properties @ fnet_interfaces_ext::Properties {
                        addresses: current_addresses,
                        id: _,
                        name: _,
                        device_class: _,
                        online: _,
                        has_default_ipv4_route: _,
                        has_default_ipv6_route: _,
                    },
            } => {
                // TODO(https://fxbug.dev/110445): Don't register interest in
                // valid-until instead of filtering out address property
                // changes manually here.
                if online.is_some()
                    || has_default_ipv4_route.is_some()
                    || has_default_ipv6_route.is_some()
                    || addresses.map_or(false, |addresses| {
                        let previous = addresses
                            .iter()
                            .filter_map(|fnet_interfaces::Address { addr, .. }| addr.as_ref());
                        let current = current_addresses
                            .iter()
                            .map(|fnet_interfaces_ext::Address { addr, valid_until: _ }| addr);
                        previous.ne(current)
                    })
                {
                    Self::compute_state(
                        &mut self.monitor,
                        &mut self.watchdog,
                        &stack,
                        properties,
                        &self.neighbor_cache,
                    )
                    .await;
                }
            }
            fnet_interfaces_ext::UpdateResult::Removed(properties) => {
                self.watchdog.handle_interface_removed(properties.id);
                self.monitor.handle_interface_removed(properties);
            }
            fnet_interfaces_ext::UpdateResult::NoChange => {}
        }
        Ok(None)
    }

    async fn compute_state(
        monitor: &mut Monitor,
        watchdog: &mut Watchdog,
        stack: &fnet_stack::StackProxy,
        properties: &fnet_interfaces_ext::Properties,
        neighbor_cache: &NeighborCache,
    ) {
        let routes = stack.get_forwarding_table().await.unwrap_or_else(|e| {
            error!("failed to get route table: {}", e);
            Vec::new()
        });

        let view = InterfaceView {
            properties,
            routes: &routes[..],
            neighbors: neighbor_cache.get_interface_neighbors(properties.id),
        };

        let monitor_fut = monitor.compute_state(view);

        let watchdog_fut =
            watchdog.check_interface_state(zx::Time::get_monotonic(), &SystemDispatcher {}, view);

        let ((), ()) = futures::future::join(monitor_fut, watchdog_fut).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use futures::task::Poll;
    use net_declare::fidl_ip;

    #[fuchsia::test]
    fn test_dns_lookup_valid_response() {
        let mut exec = fasync::TestExecutor::new();

        let (proxy, mut server_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()
                .expect("failed to create LookupMarker");
        let dns_lookup_fut = dns_lookup(&proxy);
        pin_mut!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut server_end_fut = server_stream.try_next();
        let _ = assert_matches!(exec.run_until_stalled(&mut server_end_fut),
            Poll::Ready(Ok(Some(fnet_name::LookupRequest::LookupIp { responder, hostname, .. }))) => {
                if DNS_DOMAIN == hostname {
                    responder.send(&mut Ok(fnet_name::LookupResult
                    { addresses: Some(vec![fidl_ip!("1.2.3.1")]), ..fnet_name::LookupResult::EMPTY }) )
                } else {
                    responder.send(&mut Err(fnet_name::LookupError::NotFound))
                }
            }
        );

        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Pending);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Ready(Ok(_)));
    }

    #[fuchsia::test]
    fn test_dns_lookup_net_name_error() {
        let mut exec = fasync::TestExecutor::new();

        let (proxy, mut server_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()
                .expect("failed to create LookupMarker");
        let dns_lookup_fut = dns_lookup(&proxy);
        pin_mut!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut server_end_fut = server_stream.try_next();
        let _ = assert_matches!(exec.run_until_stalled(&mut server_end_fut),
            Poll::Ready(Ok(Some(fnet_name::LookupRequest::LookupIp { responder, .. }))) => {
                // Send a not found error regardless of the hostname
                responder.send(&mut Err(fnet_name::LookupError::NotFound))
            }
        );

        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Pending);
        assert_matches!(
            exec.run_until_stalled(&mut dns_lookup_fut),
            Poll::Ready(Err(anyhow::Error { .. }))
        );
    }

    #[fuchsia::test]
    fn test_dns_lookup_timed_out() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (proxy, mut server_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()
                .expect("failed to create LookupMarker");
        let dns_lookup_fut = dns_lookup(&proxy);
        pin_mut!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut server_end_fut = server_stream.try_next();
        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Ready { .. });

        exec.set_fake_time(fasync::Time::after(FIDL_TIMEOUT));
        assert!(exec.wake_expired_timers());

        assert_matches!(
            exec.run_until_stalled(&mut dns_lookup_fut),
            Poll::Ready(Err(anyhow::Error { .. }))
        );
    }

    #[fuchsia::test]
    fn test_dns_lookup_fidl_error() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        let (proxy, _) = fidl::endpoints::create_proxy::<fnet_name::LookupMarker>()
            .expect("failed to create LookupMarker");
        let dns_lookup_fut = dns_lookup(&proxy);
        pin_mut!(dns_lookup_fut);

        assert_matches!(
            exec.run_until_stalled(&mut dns_lookup_fut),
            Poll::Ready(Err(anyhow::Error { .. }))
        );
    }
}
