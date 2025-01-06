// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the fuchsia.netpol.socketproxy.DnsServerWatcher service.

use anyhow::{Context, Error};
use fidl::endpoints::{ControlHandle as _, RequestStream as _, Responder as _};
use fidl_fuchsia_netpol_socketproxy::{self as fnp_socketproxy, DnsServerList};
use fuchsia_inspect_derive::{IValue, Inspect, Unit};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{StreamExt, TryStreamExt};
use log::{info, warn};
use std::sync::Arc;

#[derive(Unit, Debug, Default)]
struct DnsServerWatcherState {
    #[inspect(skip)]
    server_list: Vec<DnsServerList>,
    #[inspect(skip)]
    last_sent: Option<Vec<DnsServerList>>,
    #[inspect(skip)]
    queued_responder: Option<fnp_socketproxy::DnsServerWatcherWatchServersResponder>,

    updates_seen: u32,
    updates_sent: u32,
}

/// A wrapper around the fuchsia.netpol.socketproxy.DnsServerWatcher service
/// that tracks when a DnsServerList update needs to be sent.
#[derive(Inspect, Debug, Clone)]
pub(crate) struct DnsServerWatcher {
    #[inspect(forward)]
    state: Arc<Mutex<IValue<DnsServerWatcherState>>>,
    dns_rx: Arc<Mutex<mpsc::Receiver<Vec<fnp_socketproxy::DnsServerList>>>>,
}

impl DnsServerWatcher {
    /// Create a new DnsServerWatcher.
    pub(crate) fn new(
        dns_rx: Arc<Mutex<mpsc::Receiver<Vec<fnp_socketproxy::DnsServerList>>>>,
    ) -> Self {
        Self { dns_rx, state: Default::default() }
    }

    /// Runs the fuchsia.netpol.socketproxy.DnsServerWatcher service.
    pub(crate) async fn run<'a>(
        &self,
        stream: fnp_socketproxy::DnsServerWatcherRequestStream,
    ) -> Result<(), Error> {
        let mut state = match self.state.try_lock() {
            Some(o) => o,
            None => {
                warn!("Only one connection to DnsServerWatcher is allowed at a time");
                stream.control_handle().shutdown_with_epitaph(fidl::Status::ACCESS_DENIED);
                return Ok(());
            }
        };
        let mut dns_rx = self.dns_rx.lock().await;
        info!("Starting fuchsia.netpol.socketproxy.DnsServerWatcher server");
        let mut stream = stream.map(|result| result.context("failed request")).fuse();

        loop {
            futures::select! {
                request = stream.try_next() => match request? {
                    Some(fnp_socketproxy::DnsServerWatcherRequest::WatchServers { responder }) => {
                        let mut state = state.as_mut();
                        if state.queued_responder.is_some() {
                            warn!("Only one call to watch server may be active at once");
                            responder
                                .control_handle()
                                .shutdown_with_epitaph(fidl::Status::ACCESS_DENIED);
                        } else {
                            state.queued_responder = Some(responder);
                            state.maybe_respond()?;
                        }
                    },
                    None => {}
                },
                dns_update = dns_rx.select_next_some() => {
                    let mut state = state.as_mut();
                    state.updates_seen += 1;
                    state.server_list = dns_update;
                    state.maybe_respond()?;
                }
            }
        }
    }
}

impl DnsServerWatcherState {
    fn maybe_respond(&mut self) -> Result<(), Error> {
        if self.last_sent.as_ref() != Some(&self.server_list) {
            if let Some(responder) = self.queued_responder.take() {
                info!("Sending DNS update to client: {}", self.server_list.len());
                responder.send(&self.server_list)?;
                self.updates_sent += 1;
                self.last_sent = Some(self.server_list.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_component::server::ServiceFs;
    use fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    };
    use fuchsia_inspect_derive::WithInspect;
    use futures::channel::mpsc::{Receiver, Sender};
    use futures::SinkExt as _;
    use pretty_assertions::assert_eq;

    enum IncomingService {
        DnsServerWatcher(fnp_socketproxy::DnsServerWatcherRequestStream),
    }

    async fn run_registry(
        handles: LocalComponentHandles,
        dns_rx: Arc<Mutex<Receiver<Vec<fnp_socketproxy::DnsServerList>>>>,
    ) -> Result<(), Error> {
        let mut fs = ServiceFs::new();
        let _ = fs.dir("svc").add_fidl_service(IncomingService::DnsServerWatcher);
        let _ = fs.serve_connection(handles.outgoing_dir)?;

        let watcher = DnsServerWatcher::new(dns_rx)
            .with_inspect(fuchsia_inspect::component::inspector().root(), "dns_watcher")?;

        fs.for_each_concurrent(0, |IncomingService::DnsServerWatcher(stream)| {
            let watcher = watcher.clone();
            async move {
                watcher
                    .run(stream)
                    .await
                    .context("Failed to serve request stream")
                    .unwrap_or_else(|e| eprintln!("Error encountered: {e:?}"))
            }
        })
        .await;

        Ok(())
    }

    async fn setup_test(
    ) -> Result<(RealmInstance, Sender<Vec<fnp_socketproxy::DnsServerList>>), Error> {
        let builder = RealmBuilder::new().await?;
        let (dns_tx, dns_rx) = mpsc::channel(1);
        let dns_rx = Arc::new(Mutex::new(dns_rx));
        let registry = builder
            .add_local_child(
                "dns_watcher",
                {
                    let dns_rx = dns_rx.clone();
                    move |handles: LocalComponentHandles| {
                        Box::pin(run_registry(handles, dns_rx.clone()))
                    }
                },
                ChildOptions::new(),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fnp_socketproxy::DnsServerWatcherMarker>())
                    .from(&registry)
                    .to(Ref::parent()),
            )
            .await?;

        let realm = builder.build().await?;

        Ok((realm, dns_tx))
    }

    #[fuchsia::test]
    async fn test_normal_operation() -> Result<(), Error> {
        let (realm, mut dns_tx) = setup_test().await?;

        let dns_server_watcher = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
            .context("While connecting to DnsServerWatcher")?;

        // Initial watch should return immediately
        assert_eq!(dns_server_watcher.watch_servers().await?, vec![]);

        // Send a new DNS update
        let (send_result, watch_result) = futures::future::join(
            dns_tx.send(vec![DnsServerList { source_network_id: Some(0), ..Default::default() }]),
            dns_server_watcher.watch_servers(),
        )
        .await;

        assert_matches!(send_result, Ok(()));
        assert_eq!(
            watch_result?,
            vec![DnsServerList { source_network_id: Some(0), ..Default::default() }]
        );

        assert_data_tree!(fuchsia_inspect::component::inspector(), root: {
            dns_watcher: {
                updates_seen: 1u64,
                updates_sent: 2u64,
            },
        });

        Ok(())
    }

    #[fuchsia::test]
    async fn test_duplicate_list() -> Result<(), Error> {
        let (realm, mut dns_tx) = setup_test().await?;
        let dns_server_watcher = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
            .context("While connecting to DnsServerWatcher")?;

        // Initial watch should return immediately
        assert_eq!(dns_server_watcher.watch_servers().await?, vec![]);

        let server_list = vec![DnsServerList { source_network_id: Some(0), ..Default::default() }];

        let mut dns_tx2 = dns_tx.clone();
        let mut dns_tx3 = dns_tx.clone();
        let (watch_result, s1, s2, s3) = futures::join!(
            dns_server_watcher.watch_servers(),
            dns_tx.send(server_list.clone()),
            dns_tx2.send(server_list.clone()),
            dns_tx3.send(server_list.clone()),
        );

        assert_matches!(s1, Ok(()));
        assert_matches!(s2, Ok(()));
        assert_matches!(s3, Ok(()));
        assert_eq!(watch_result?, server_list);

        // Send a new (distinct) DNS update
        let (send_result, watch_result) = futures::future::join(
            dns_tx.send(vec![DnsServerList { source_network_id: Some(1), ..Default::default() }]),
            dns_server_watcher.watch_servers(),
        )
        .await;
        assert_matches!(send_result, Ok(()));

        // We expect that this watch should get the new server list, not one of
        // the old duplicate ones.
        assert_eq!(
            watch_result?,
            vec![DnsServerList { source_network_id: Some(1), ..Default::default() }]
        );

        assert_data_tree!(fuchsia_inspect::component::inspector(), root: {
            dns_watcher: {
                updates_seen: 4u64,
                updates_sent: 3u64,
            },
        });

        Ok(())
    }

    #[fuchsia::test]
    async fn test_duplicate_watch() -> Result<(), Error> {
        let (realm, _dns_tx) = setup_test().await?;

        let dns_server_watcher = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::DnsServerWatcherMarker>()
            .context("While connecting to DnsServerWatcher")?;

        // Initial watch should return immediately
        assert_eq!(dns_server_watcher.watch_servers().await?, vec![]);

        let watch1 = dns_server_watcher.watch_servers();
        let watch2 = dns_server_watcher.watch_servers();

        // Two simultaneous calls to watch_servers is invalid and will cause the
        // watcher channel to be closed.
        assert_matches!(
            futures::future::join(watch1, watch2).await,
            (
                Err(fidl::Error::ClientChannelClosed { status: fidl::Status::ACCESS_DENIED, .. }),
                Err(fidl::Error::ClientChannelClosed { status: fidl::Status::ACCESS_DENIED, .. })
            )
        );

        assert_data_tree!(fuchsia_inspect::component::inspector(), root: {
            dns_watcher: {
                updates_seen: 0u64,
                updates_sent: 1u64,
            },
        });

        Ok(())
    }
}
