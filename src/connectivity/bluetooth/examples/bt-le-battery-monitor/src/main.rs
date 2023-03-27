// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_bluetooth_le::{
    CentralMarker, CentralProxy, Filter, Peer, ScanOptions, ScanResultWatcherMarker,
    ScanResultWatcherProxy,
};
use fuchsia_bluetooth::types::le::Peer as btPeer;
use fuchsia_bluetooth::types::Uuid;
use fuchsia_component::client::connect_to_protocol;
use futures::stream::TryStreamExt;
use tracing::info;

/// The UUID of with the GATT battery service.
const BATTERY_SERVICE_UUID: Uuid = Uuid::new16(0x180f);

/// Processes the scan `result` and logs the compatible peers.
async fn handle_scan_result(_central: &CentralProxy, result: Vec<Peer>) -> Result<(), Error> {
    // Try each peer until one succeeds.
    for peer in result {
        let peer = btPeer::try_from(peer)?;
        info!(id = %peer.id, "Found compatible peer");
    }
    Ok(())
}

async fn watch_scan_results(
    central: CentralProxy,
    watcher: ScanResultWatcherProxy,
) -> Result<(), Error> {
    let mut scan_result_stream =
        HangingGetStream::new(watcher.clone(), ScanResultWatcherProxy::watch);

    while let Some(scan_result) = scan_result_stream.try_next().await? {
        handle_scan_result(&central, scan_result).await?;
    }
    info!("Scan result watcher finished");
    Ok(())
}

#[fuchsia::main(logging_tags = ["bt-le-battery-monitor"])]
async fn main() -> Result<(), Error> {
    info!("Starting LE Battery Monitor");
    let central = connect_to_protocol::<CentralMarker>()?;
    let (scan_client, scan_server) = fidl::endpoints::create_proxy::<ScanResultWatcherMarker>()?;
    // Only scan for devices with the Battery Service.
    let options = ScanOptions {
        filters: Some(vec![Filter {
            service_uuid: Some(BATTERY_SERVICE_UUID.into()),
            connectable: Some(true),
            ..Filter::EMPTY
        }]),
        ..ScanOptions::EMPTY
    };
    // The lifetime of the scan will be determined by `watch_scan_results` so this Future can be
    // ignored.
    let _scan_fut = central.scan(options, scan_server).check()?;
    let scan_result = watch_scan_results(central, scan_client).await;
    info!("LE Battery Monitor finished: {scan_result:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use async_test_helpers::run_while;
    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use futures::{pin_mut, StreamExt};

    #[fuchsia::test]
    fn watch_scan_results_lifetime() {
        let mut exec = fasync::TestExecutor::new();
        let (central, _central_server) =
            fidl::endpoints::create_proxy_and_stream::<CentralMarker>().unwrap();
        let (watch_client, mut watch_server) =
            fidl::endpoints::create_proxy_and_stream::<ScanResultWatcherMarker>().unwrap();

        let watch_fut = watch_scan_results(central.clone(), watch_client);
        let server_fut = watch_server.next();
        pin_mut!(watch_fut, server_fut);
        // Should receive initial request to watch.
        let (result, mut watch_fut) = run_while(&mut exec, watch_fut, server_fut);
        let responder = result.unwrap().expect("fidl request").into_watch().unwrap();

        let peers: Vec<Peer> = vec![];
        let _ = responder.send(&mut peers.into_iter());
        // If the ScanResultWatcher server terminates, then the watcher future should terminate.
        drop(watch_server);

        // After running the watcher, it will register a new call to `watch` and will fail because
        // the server is gone.
        let result = exec.run_until_stalled(&mut watch_fut).expect("finished");
        assert_matches!(result, Err(_));
    }
}
