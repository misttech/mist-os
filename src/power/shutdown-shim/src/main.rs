// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints;
use fidl_fuchsia_io as fio;

#[fuchsia::main]
async fn main() {
    let Some(directory_request) =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
    else {
        eprintln!("[shutdown-shim]: could not obtain directory request handle");
        std::process::exit(1);
    };
    let (svc, server) = endpoints::create_proxy::<fio::DirectoryMarker>();
    let Ok(()) = fdio::open("/svc", fio::PERM_READABLE, server.into()) else {
        eprintln!("[shutdown-shim]: could not obtain namespace");
        std::process::exit(1);
    };
    if let Err(err) = shutdown_shim::main(svc, directory_request.into()).await {
        eprintln!("[shutdown-shim]: {err}");
        std::process::exit(1);
    }
}
