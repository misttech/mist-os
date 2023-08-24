// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This program serves the fuchsia.archivist.test.Puppet protocol.
//
// It is meant to be controlled by a test suite and will emit log messages
// and inspect data as requested. This output can be retrieved from the
// archivist under test using fuchsia.diagnostics.ArchiveAccessor.
//
// For full documentation, see //src/diagnostics/archivist/testing/realm-factory/README.md

use anyhow::Error;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::{StreamExt, TryStreamExt};

use fidl_fuchsia_archivist_test as fpuppet;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    inspect_runtime::serve(component::inspector(), &mut fs)?;

    fs.dir("svc").add_fidl_service(|stream: fpuppet::PuppetRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_puppet).await;
    Ok(())
}

async fn serve_puppet(mut stream: fpuppet::PuppetRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        handle_puppet_request(request).unwrap();
    }
}

fn handle_puppet_request(request: fpuppet::PuppetRequest) -> Result<(), Error> {
    match request {
        fpuppet::PuppetRequest::SetHealthOk { responder } => {
            component::health().set_ok();
            responder.send()?;
            Ok(())
        }
        fpuppet::PuppetRequest::Println { message, .. } => {
            println!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::Eprintln { message, .. } => {
            eprintln!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::_UnknownMethod { .. } => unreachable!(),
    }
}
