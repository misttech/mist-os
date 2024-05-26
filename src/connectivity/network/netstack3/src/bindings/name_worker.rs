// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ProtocolMarker as _;
use fidl_fuchsia_net_name::{self as fnet_name, DnsServerWatcherRequestStream};

use tracing::warn;

use crate::bindings::Netstack;

pub(super) async fn serve(
    _ns: Netstack,
    _stream: DnsServerWatcherRequestStream,
) -> Result<(), fidl::Error> {
    // TODO(https://fxbug.dev/341759423): Expose NDP-acquired DNS server
    // configuration from netstack3.
    warn!("blocking forever serving {}", fnet_name::DnsServerWatcherMarker::DEBUG_NAME);
    futures::future::pending().await
}
