// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use anyhow::{Context as _, Error};
use fuchsia_async::{self as fasync, DurationExt as _};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};
use futures::lock::Mutex;
use futures::FutureExt as _;
use net_declare::{fidl_ip, fidl_socket_addr};
use pretty_assertions::assert_eq;
use socket_proxy_testing::{ToDnsServerList as _, ToNetwork as _};
use std::sync::Arc;
use {fidl_fuchsia_netpol_socketproxy as fnp_socketproxy, fuchsia_zircon as zx};

#[fuchsia::test]
async fn watch_dns_and_use_registry() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;
    let socket_proxy = builder
        .add_child("socket_proxy", "#meta/network-socket-proxy.cm", ChildOptions::new().eager())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                .capability(Capability::protocol::<fnp_socketproxy::DnsServerWatcherMarker>())
                .from(&socket_proxy)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let dns_watcher = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
        .context("trying to connect to DNS Server watcher")?;

    let starnix_networks = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
        .context("trying to connect to StarnixNetworks")?;

    let dns_history = Arc::new(Mutex::new(Vec::new()));

    let pause = || fasync::Timer::new(zx::Duration::from_millis(1).after_now());

    // `select_all` runs a Vec of futures with identical return values until one of them completes.
    // Once the first future resolves, it returns a tuple containing the result of the completed
    // future, the index of that future in the original Vec, and a Vec containing the remaining
    // incomplete futures.
    let (result, ix, remaining): (Result<(), Error>, _, _) = futures::future::select_all(vec![
        {
            let dns_history = dns_history.clone();
            async move {
                loop {
                    let mut update = dns_watcher.watch_servers().await?;
                    update.sort_by_key(|a| a.source_network_id);
                    dns_history.lock().await.push(update);
                }
            }
            .boxed()
        },
        async move {
            // Add network 1 with 1 v4 address
            assert_eq!(
                starnix_networks.add(&(1, vec![fidl_ip!["192.0.2.0"]]).to_network()).await?,
                Ok(())
            );
            pause().await;

            // Add network 2 with 1 v6 address
            assert_eq!(
                starnix_networks.add(&(2, vec![fidl_ip!["2001:db8::3"]]).to_network()).await?,
                Ok(())
            );
            pause().await;

            // Update network 2 with the same information. Should not cause any DNS updates
            assert_eq!(
                starnix_networks.update(&(2, vec![fidl_ip!["2001:db8::3"]]).to_network()).await?,
                Ok(())
            );
            pause().await;

            // Update network 1 so that it has 1 v4 address and 1 v6 address.
            assert_eq!(
                starnix_networks
                    .update(&(1, vec![fidl_ip!["192.0.2.1"], fidl_ip!["2001:db8::4"]]).to_network())
                    .await?,
                Ok(())
            );
            pause().await;

            // Remove network 2.
            assert_eq!(starnix_networks.remove(2).await?, Ok(()));
            pause().await;
            Ok(())
        }
        .boxed(),
    ])
    .await;

    // Make sure the starnix_networks future completed successfully.
    result?;
    // Ensure that the expected future resolved.
    assert_eq!(ix, 1);
    // Ensure that the DNS Watcher future is not finished.
    assert_eq!(remaining.len(), 1);

    assert_eq!(
        *dns_history.lock().await,
        vec![
            vec![],
            vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")]).to_dns_server_list()],
            vec![
                (1, vec![fidl_socket_addr!("192.0.2.0:53")]).to_dns_server_list(),
                (2, vec![fidl_socket_addr!("[2001:db8::3]:53")]).to_dns_server_list(),
            ],
            vec![
                (1, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::4]:53")])
                    .to_dns_server_list(),
                (2, vec![fidl_socket_addr!("[2001:db8::3]:53")]).to_dns_server_list(),
            ],
            vec![(
                1,
                vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::4]:53")]
            )
                .to_dns_server_list()],
        ]
    );

    Ok(())
}
