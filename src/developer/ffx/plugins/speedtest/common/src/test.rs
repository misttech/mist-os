// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU32;
use std::time::Duration;

use futures::TryStreamExt as _;
use test_case::test_case;
use {fidl_fuchsia_developer_ffx_speedtest as fspeedtest, fuchsia_async as fasync};

use crate::client::{self, PingReport, SocketTransferParams, SocketTransferReport};
use crate::server;

async fn with_client(f: impl AsyncFnOnce(client::Client)) {
    let (client, mut server_rs) =
        fidl::endpoints::create_proxy_and_stream::<fspeedtest::SpeedtestMarker>();
    let scope = fasync::Scope::new();
    let handle = scope.to_handle();
    let server = server::Server::new(scope);
    let _ = handle.spawn(async move {
        while let Some(req) = server_rs.try_next().await.expect("serve error") {
            server.handle_request(req).expect("handle request error");
        }
    });
    let client = client::Client::new(client).await.expect("client creation error");
    f(client).await;
    handle.on_no_tasks().await;
}

#[fasync::run_singlethreaded(test)]
async fn ping() {
    with_client(async |client| {
        let PingReport { max, avg, min } =
            client.ping(NonZeroU32::new(3).unwrap()).await.expect("ping failed");
        // We can't fake time in host tests so just ensure we're seeing nonzero
        // here.
        assert!(min > Duration::ZERO);
        assert!(avg >= min);
        assert!(max >= avg);
    })
    .await;
}

#[test_case(client::Direction::Tx; "tx")]
#[test_case(client::Direction::Rx; "rx")]
#[fasync::run_singlethreaded(test)]
async fn socket(direction: client::Direction) {
    with_client(async |client| {
        let data_len = NonZeroU32::new(10_000).unwrap();
        let SocketTransferReport { direction: got_direction, client, server } = client
            .socket(SocketTransferParams {
                direction,
                params: client::TransferParams {
                    data_len,
                    buffer_len: NonZeroU32::new(fspeedtest::DEFAULT_BUFFER_SIZE).unwrap(),
                },
            })
            .await
            .expect("test failed");

        assert_eq!(got_direction, direction);
        assert_eq!(client.transfer_len, data_len);
        assert_eq!(server.transfer_len, data_len);
        // We can't fake time in host tests so just ensure we're seeing nonzero
        // here.
        assert_ne!(client.duration, Duration::ZERO);
        assert_ne!(server.duration, Duration::ZERO);
    })
    .await;
}
