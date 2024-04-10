// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::FIDL_TIMEOUT_ID, anyhow::anyhow, async_trait::async_trait, fidl_fuchsia_net as fnet,
    fidl_fuchsia_net_name as fnet_name, fuchsia_zircon as zx, futures::TryFutureExt,
    named_timer::NamedTimeoutExt, tracing::warn,
};

const DNS_FIDL_TIMEOUT: zx::Duration = zx::Duration::from_seconds(90);

async fn dig<F: Fn() -> anyhow::Result<fnet_name::LookupProxy>>(
    name_lookup_connector: &F,
    // TODO(https://fxbug.dev/42182624): DNS fetching should be done over the
    // specified interface. The current implementation of
    // fidl::fuchsia::net::name::LookupIp does not support this.
    _interface_name: &str,
    domain: &str,
) -> anyhow::Result<crate::ResolvedIps> {
    let name_lookup = name_lookup_connector()?;
    let results = name_lookup
        .lookup_ip(
            domain,
            &fnet_name::LookupIpOptions {
                ipv4_lookup: Some(true),
                ipv6_lookup: Some(true),
                ..Default::default()
            },
        )
        .map_err(|e: fidl::Error| anyhow!("lookup_ip call failed: {e:?}"))
        .on_timeout_named(&FIDL_TIMEOUT_ID, DNS_FIDL_TIMEOUT, || {
            Err(anyhow!("lookup_ip timed out after {} seconds", DNS_FIDL_TIMEOUT.into_seconds()))
        })
        .await
        .map_err(|e: anyhow::Error| anyhow!("{e:?}"))?
        .map_err(|e: fnet_name::LookupError| anyhow!("lookup failed: {e:?}"))?;

    if let Some(addresses) = results.addresses {
        let mut resolved = crate::ResolvedIps::default();
        for address in addresses {
            match address {
                fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr }) => resolved.v4.push(addr.into()),
                fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr }) => resolved.v6.push(addr.into()),
            }
        }
        Ok(resolved)
    } else {
        Err(anyhow!("no ip addresses were resolved"))
    }
}

#[async_trait]
pub trait Dig {
    async fn dig(&self, interface_name: &str, domain: &str) -> Option<crate::ResolvedIps>;
}

pub struct Digger<F>(F);

impl Digger<()> {
    pub fn new() -> Digger<impl Fn() -> anyhow::Result<fnet_name::LookupProxy>> {
        Digger(|| Ok(fuchsia_component::client::connect_to_protocol::<fnet_name::LookupMarker>()?))
    }
}

#[async_trait]
impl<F: Fn() -> anyhow::Result<fnet_name::LookupProxy> + std::marker::Sync> Dig for Digger<F> {
    async fn dig(&self, interface_name: &str, domain: &str) -> Option<crate::ResolvedIps> {
        let r = dig(&self.0, interface_name, &domain).await;
        match r {
            Ok(ips) => Some(ips),
            Err(e) => {
                warn!("error while digging {domain}: {e:?}");
                None
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use {
        assert_matches::assert_matches,
        fuchsia_async as fasync,
        futures::{prelude::*, task::Poll},
        net_declare::fidl_ip,
        std::{
            pin::pin,
            sync::{Arc, Mutex},
        },
    };

    const DNS_DOMAIN: &str = "www.gstatic.com";

    #[fuchsia::test]
    fn test_dns_lookup_valid_response() {
        let mut exec = fasync::TestExecutor::new();
        let server_stream = Arc::new(Mutex::new(None));
        let stream_ref = server_stream.clone();

        let digger = Digger(move || {
            let (proxy, server) =
                fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()?;
            *stream_ref.lock().unwrap() = Some(server);
            Ok(proxy)
        });
        let dns_lookup_fut = digger.dig("", DNS_DOMAIN);
        let mut dns_lookup_fut = pin!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut locked = server_stream.lock().unwrap();
        let mut server_end_fut = locked.as_mut().unwrap().try_next();
        let _ = assert_matches!(exec.run_until_stalled(&mut server_end_fut),
            Poll::Ready(Ok(Some(fnet_name::LookupRequest::LookupIp { responder, hostname, .. }))) => {
                if DNS_DOMAIN == hostname {
                    responder.send(Ok(&fnet_name::LookupResult {
                        addresses: Some(vec![fidl_ip!("1.2.3.1")]), ..Default::default()
                    }))
                } else {
                    responder.send(Err(fnet_name::LookupError::NotFound))
                }
            }
        );

        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Pending);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Ready(Some(_)));
    }

    #[fuchsia::test]
    fn test_dns_lookup_net_name_error() {
        let mut exec = fasync::TestExecutor::new();

        let server_stream = Arc::new(Mutex::new(None));
        let stream_ref = server_stream.clone();

        let digger = Digger(move || {
            let (proxy, server) =
                fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()?;
            *stream_ref.lock().unwrap() = Some(server);
            Ok(proxy)
        });
        let dns_lookup_fut = digger.dig("", DNS_DOMAIN);
        let mut dns_lookup_fut = pin!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut locked = server_stream.lock().unwrap();
        let mut server_end_fut = locked.as_mut().unwrap().try_next();
        let _ = assert_matches!(exec.run_until_stalled(&mut server_end_fut),
            Poll::Ready(Ok(Some(fnet_name::LookupRequest::LookupIp { responder, .. }))) => {
                // Send a not found error regardless of the hostname
                responder.send(Err(fnet_name::LookupError::NotFound))
            }
        );

        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Pending);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Ready(None));
    }

    #[fuchsia::test]
    fn test_dns_lookup_timed_out() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let server_stream = Arc::new(Mutex::new(None));
        let stream_ref = server_stream.clone();

        let digger = Digger(move || {
            let (proxy, server) =
                fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()?;
            *stream_ref.lock().unwrap() = Some(server);
            Ok(proxy)
        });
        let dns_lookup_fut = digger.dig("", DNS_DOMAIN);
        let mut dns_lookup_fut = pin!(dns_lookup_fut);
        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Pending);

        let mut locked = server_stream.lock().unwrap();
        let mut server_end_fut = locked.as_mut().unwrap().try_next();
        assert_matches!(exec.run_until_stalled(&mut server_end_fut), Poll::Ready { .. });

        exec.set_fake_time(fasync::Time::after(DNS_FIDL_TIMEOUT));
        assert!(exec.wake_expired_timers());

        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Ready(None));
    }

    #[fuchsia::test]
    fn test_dns_lookup_fidl_error() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        let digger = Digger(move || {
            let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<fnet_name::LookupMarker>()?;
            Ok(proxy)
        });
        let dns_lookup_fut = digger.dig("", DNS_DOMAIN);
        let mut dns_lookup_fut = pin!(dns_lookup_fut);

        assert_matches!(exec.run_until_stalled(&mut dns_lookup_fut), Poll::Ready(None));
    }
}
