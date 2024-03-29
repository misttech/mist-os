// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod ta_loader;
mod trusted_app;

use std::{ffi::CString, fs::File, io::Read};

use anyhow::{Context as _, Error};
use fidl_fuchsia_tee::{ApplicationRequest, ApplicationRequestStream};
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;

fn read_ta_name() -> Result<CString, Error> {
    let mut f = File::open("/pkg/data/ta_name")
        .map_err(|e| anyhow::anyhow!("Could not open ta_name file: {e}"))?;
    let mut s = String::from("lib");
    let _ = f
        .read_to_string(&mut s)
        .map_err(|e| anyhow::anyhow!("Could not read ta_name file: {e}"))?;
    s.push_str(".so");
    CString::new(s).map_err(|_| anyhow::anyhow!("nul found in name string"))
}

async fn run_application(stream: ApplicationRequestStream) -> Result<(), Error> {
    let name = read_ta_name()?;
    let interface = ta_loader::load_ta(&name)?;

    let ta = trusted_app::TrustedApp::new(interface)?;

    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                ApplicationRequest::OpenSession2 { .. } => {
                    todo!();
                }
                ApplicationRequest::CloseSession { .. } => {
                    todo!();
                }
                ApplicationRequest::InvokeCommand { .. } => {
                    // TODO: Call ta.invoke_command
                    todo!();
                }
            }
        })
        .await?;
    ta.destroy();
    Ok(())
}

enum IncomingService {
    Application(ApplicationRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    let _ = fs.dir("svc").add_fidl_service(IncomingService::Application);

    let _ = fs.take_and_serve_directory_handle()?;

    fs.for_each(|IncomingService::Application(stream)| async {
        run_application(stream).await.unwrap()
    })
    .await;

    // Generate link-time references to the symbols that we want to expose to
    // the TA so that the definitions will be retained by the linker.
    let _ = std::hint::black_box(tee_internal_impl::binding_stubs::exposed_c_entry_points());

    Ok(())
}
