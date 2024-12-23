// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude::*;
use fidl::client::QueryResponseFut;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_net_dhcpv6::{
    AcquirePrefixConfig, PrefixControlMarker, PrefixControlProxy, PrefixEvent, PrefixProviderMarker,
};
use fuchsia_async::Task;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_sync::Mutex;

use std::net::Ipv6Addr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

const PREFIX_LEN: u8 = 64;

#[derive(Debug, Default, Clone)]
pub struct DhcpV6Pd {
    inner: Arc<Mutex<DhcpV6PdInner>>,
}

#[derive(Debug, Default)]
pub struct DhcpV6PdInner {
    prefix_control: Option<PrefixControlProxy>,
    prefix_watch: Option<QueryResponseFut<PrefixEvent>>,
    prefix: Option<ot::Ip6Prefix>,
    valid: zx::MonotonicInstant,
    preferred: zx::MonotonicInstant,
    last_state: ot::BorderRoutingDhcp6PdState,
    waker: Option<Waker>,
    refresh_task: Option<Task<()>>,
}

fn convert_zx_time_into_seconds_until(time: zx::MonotonicInstant) -> u32 {
    let duration = time - zx::MonotonicInstant::get();

    if duration == zx::MonotonicDuration::INFINITE {
        u32::MAX
    } else if duration <= zx::MonotonicDuration::ZERO {
        u32::MIN
    } else {
        duration.into_seconds().try_into().unwrap_or(u32::MAX)
    }
}

fn make_fake_ra_prefix_packet(prefix: ot::Ip6Prefix, valid: u32, preferred: u32) -> Vec<u8> {
    use net_types::ip::Ipv6;
    use packet::{InnerPacketBuilder, Serializer};
    use packet_formats::icmp::ndp::options::{NdpOptionBuilder, PrefixInformation};
    use packet_formats::icmp::ndp::{OptionSequenceBuilder, RoutePreference, RouterAdvertisement};
    use packet_formats::icmp::{IcmpPacketBuilder, IcmpZeroCode};

    let src_addr = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1); // Local host address
    let dst_addr = Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 2); // All routers multicast
    let hop_limit = 100;
    let managed_flag = false;
    let other_config_flag = false;
    let router_lifetime_seconds = u16::MAX;
    let reachable_time_seconds = u32::MAX;
    let retransmit_timer_seconds = u32::MAX;

    // Note: These fields are ignored by OpenThread, so these are placeholders.
    let ra = RouterAdvertisement::with_prf(
        hop_limit,
        managed_flag,
        other_config_flag,
        RoutePreference::default(),
        router_lifetime_seconds,
        reachable_time_seconds,
        retransmit_timer_seconds,
    );

    let prefix_information = PrefixInformation::new(
        prefix.prefix_len(),
        true, // On-Link, Ignored by OpenThread
        true, // Autonomous, Ignored by OpenThread
        valid,
        preferred,
        (*prefix.addr()).into(),
    );

    let options = &[NdpOptionBuilder::PrefixInformation(prefix_information)];

    let serialized = OptionSequenceBuilder::new(options.iter())
        .into_serializer()
        .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(src_addr, dst_addr, IcmpZeroCode, ra))
        .serialize_vec_outer()
        .unwrap()
        .as_ref()
        .to_vec();

    serialized
}

// Updates OpenThread about the added prefix
fn dhcp_v6_pd_prefix_assigned<T: ot::BorderRouter>(
    instance: &T,
    prefix: ot::Ip6Prefix,
    valid: zx::MonotonicInstant,
    preferred: zx::MonotonicInstant,
) {
    // Here we need to construct a fake ICMPv6 RA
    // that we can feed into OpenThread to let it
    // know about our delegated prefix via DHCPv6-PD.

    let valid = convert_zx_time_into_seconds_until(valid);
    let preferred = convert_zx_time_into_seconds_until(preferred);
    let fake_ra = make_fake_ra_prefix_packet(prefix, valid, preferred);

    instance
        .border_routing_process_icmp6_ra(&fake_ra)
        .expect("Wrong size returned from border_routing_process_icmp6_ra");
}

// Updates OpenThread about the removed prefix
fn dhcp_v6_pd_prefix_unassigned<T: ot::BorderRouter>(instance: &T, prefix: ot::Ip6Prefix) {
    // Here we need to construct a fake ICMPv6 RA
    // that we can feed into OpenThread to let it
    // know that our previously delegated prefix
    // is no longer assigned to us.
    let fake_ra = make_fake_ra_prefix_packet(prefix, 0, 0);

    instance
        .border_routing_process_icmp6_ra(&fake_ra)
        .expect("Wrong size returned from border_routing_process_icmp6_ra");
}

impl DhcpV6PdInner {
    fn abandon_current_prefix(&mut self, instance: &ot::Instance) {
        if let Some(prefix) = self.prefix {
            info!(tag = "dhcp_v6_pd"; "Abandoning current prefix `{}`", prefix);

            dhcp_v6_pd_prefix_unassigned(instance, prefix);

            self.prefix = None;
        }
    }

    fn update_current_prefix(&mut self, instance: &ot::Instance) {
        if let Some(prefix) = self.prefix {
            dhcp_v6_pd_prefix_assigned(instance, prefix, self.valid, self.preferred);
        }
    }
}

impl DhcpV6Pd {
    pub fn process_pd_state_change(&self, state: ot::BorderRoutingDhcp6PdState) -> Result {
        let mut inner = self.inner.lock();
        if state != inner.last_state {
            inner.last_state = state;

            // We need to re-lock the inner before calling
            // start or stop, otherwise we will deadlock.
            std::mem::drop(inner);

            match state {
                ot::BorderRoutingDhcp6PdState::Running => self.start()?,
                _ => self.stop(),
            }
        }
        Ok(())
    }

    pub fn start(&self) -> Result<(), anyhow::Error> {
        self.stop(); // ensure stop in case the online task is not terminated

        info!(tag = "dhcp_v6_pd"; "Starting attempt to lease a prefix via DHCPv6-PD...");
        let prefix_provider =
            connect_to_protocol::<PrefixProviderMarker>().context("dhcpv6pd.start")?;

        let (client, server) = create_endpoints::<PrefixControlMarker>();

        prefix_provider
            .acquire_prefix(
                &AcquirePrefixConfig {
                    preferred_prefix_len: Some(PREFIX_LEN),
                    ..AcquirePrefixConfig::default()
                },
                server,
            )
            .context("dhcpv6pd.start")?;

        let prefix_control = client.into_proxy();
        let watcher = prefix_control.watch_prefix();

        let mut inner = self.inner.lock();
        inner.prefix_control = Some(prefix_control);
        inner.prefix_watch = Some(watcher);

        // Make sure our `poll()` method gets called.
        inner.waker.take().and_then(|waker| {
            waker.wake();
            Some(())
        });

        let inner_clone = self.inner.clone();
        inner.refresh_task = Some(Task::spawn(async move {
            // This loop will make sure that our poll method gets
            // woken up at least once every 15 minutes. This
            // helps make sure that the prefix update is in good shape.

            // Set our refresh duration to 15 minutes.
            const REFRESH_DURATION: fuchsia_async::MonotonicDuration =
                fuchsia_async::MonotonicDuration::from_minutes(15);

            loop {
                // Wait for the refresh duration.
                fuchsia_async::Timer::new(REFRESH_DURATION).await;

                // Lock our inner.
                let mut inner = inner_clone.lock();

                info!(tag = "dhcp_v6_pd"; "Refreshing DHCPv6-PD RA for OpenThread");

                // Make sure our `poll()` method gets called by waking up the waker.
                inner.waker.take().and_then(|waker| {
                    waker.wake();
                    Some(())
                });
            }
        }));

        Ok(())
    }

    pub fn stop(&self) {
        let mut inner = self.inner.lock();

        // To stop, we simply dispose of the control endpoint for the prefix.
        // The prefix will be removed the next time `poll()` is called.
        if let Some(_) = inner.prefix_control.take() {
            info!(tag = "dhcp_v6_pd"; "STOPPING attempt to lease a prefix via DHCPv6-PD.");
            // Make sure our `poll()` method gets called.
            inner.waker.take().and_then(|waker| {
                waker.wake();
                Some(())
            });
        }

        inner.refresh_task = None;
    }

    /// Async entrypoint. Called from [`DhcpV6PdPollerExt::dhcp_v6_pd_poll`].
    fn poll(&self, instance: &ot::Instance, cx: &mut Context<'_>) -> std::task::Poll<Result> {
        let mut inner = self.inner.lock();
        inner.waker.replace(cx.waker().clone());

        match (inner.prefix_control.clone(), inner.prefix) {
            (None, None) => {
                // Do nothing in this case.
            }

            (Some(prefix_control), _) => loop {
                // We have a prefix control. This code will
                // loop until the prefix watch no longer returns
                // `Poll::Pending`.
                if inner.prefix_watch.is_some() {
                    match inner.prefix_watch.as_mut().unwrap().poll_unpin(cx) {
                        Poll::Ready(Ok(PrefixEvent::Unassigned(_))) => {
                            info!(tag = "dhcp_v6_pd"; "DHCPv6 Prefix Unassigned");
                            inner.abandon_current_prefix(instance);
                        }
                        Poll::Ready(Ok(PrefixEvent::Assigned(prefix))) => {
                            let prefix_prefix = ot::Ip6Prefix::from(prefix.prefix);

                            if inner.prefix.is_some() && inner.prefix != Some(prefix_prefix) {
                                inner.abandon_current_prefix(instance);
                            }

                            inner.prefix = Some(prefix_prefix);
                            inner.valid =
                                zx::MonotonicInstant::from_nanos(prefix.lifetimes.valid_until);
                            inner.preferred =
                                zx::MonotonicInstant::from_nanos(prefix.lifetimes.preferred_until);

                            info!(
                                tag = "dhcp_v6_pd";
                                "DHCPv6 Prefix Assigned: {:?}, valid: {}s, preferred: {}s",
                                prefix_prefix,
                                convert_zx_time_into_seconds_until(inner.valid),
                                convert_zx_time_into_seconds_until(inner.preferred),
                            );

                            inner.update_current_prefix(instance);
                        }
                        Poll::Ready(Err(fidl_error)) => {
                            error!(
                                tag = "dhcp_v6_pd";
                                "Error watching prefix control: {:?}", fidl_error
                            );

                            // Change our last state to "stopped" so that we can
                            // re-establish our FIDL connections.
                            inner.last_state = ot::BorderRoutingDhcp6PdState::Stopped;

                            return Poll::Ready(Err(fidl_error).context("DhcpV6Pd"));
                        }
                        Poll::Pending => {
                            inner.update_current_prefix(instance);
                            return Poll::Pending;
                        }
                    }
                }

                // Continue to watch.
                inner.prefix_watch = Some(prefix_control.watch_prefix());
            },
            (None, Some(_)) => {
                // We have no control endpoint, but we have a prefix.
                // We need to remove this prefix from the interface.
                inner.abandon_current_prefix(instance);

                // No more watching.
                inner.prefix_watch = None;
            }
        }
        Poll::Pending
    }
}

#[derive(Debug)]
pub struct DhcpV6PdPoller<'a, T: ?Sized>(&'a T);
impl<'a, T: DhcpV6PdPollerExt + ?Sized> Future for DhcpV6PdPoller<'a, T> {
    type Output = Result;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.dhcp_v6_pd_poll(cx)
    }
}

pub trait DhcpV6PdPollerExt {
    fn dhcp_v6_pd_poll(&self, cx: &mut Context<'_>) -> Poll<Result>;

    fn dhcp_v6_pd_future(&self) -> DhcpV6PdPoller<'_, Self> {
        DhcpV6PdPoller(self)
    }
}

impl<T: AsRef<ot::Instance> + AsRef<DhcpV6Pd>> DhcpV6PdPollerExt for fuchsia_sync::Mutex<T> {
    fn dhcp_v6_pd_poll(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result> {
        let guard = self.lock();

        let ot: &ot::Instance = guard.as_ref();
        let dhcp_v6_pd: &DhcpV6Pd = guard.as_ref();
        dhcp_v6_pd.poll(ot, cx)
    }
}
