// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net_name as fnet_name;
use fuchsia_zircon as zx;

use dns_server_watcher::{DnsServers, DnsServersUpdateSource};
use tracing::{trace, warn};

/// Updates the DNS servers used by the DNS resolver.
pub(super) async fn update_servers(
    lookup_admin: &fnet_name::LookupAdminProxy,
    dns_servers: &mut DnsServers,
    source: DnsServersUpdateSource,
    servers: Vec<fnet_name::DnsServer_>,
) {
    trace!("updating DNS servers obtained from {:?} to {:?}", source, servers);

    dns_servers.set_servers_from_source(source, servers);
    let servers = dns_servers.consolidated();
    trace!("updating LookupAdmin with DNS servers = {:?}", servers);

    match lookup_admin.set_dns_servers(&servers).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("error setting DNS servers: {:?}", zx::Status::from_raw(e)),
        Err(e) => warn!("error sending set DNS servers request: {:?}", e),
    }
}
