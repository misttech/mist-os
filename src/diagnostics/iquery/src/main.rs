// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_component::client;
use iquery::command_line::CommandLine;
use iquery::commands::{ArchiveAccessorProvider, Command};
use {fidl_fuchsia_sys2 as fsys2, fuchsia_async as fasync};

#[cfg(test)]
#[macro_use]
mod tests;

const ROOT_REALM_QUERY: &str = "/svc/fuchsia.sys2.RealmQuery.root";

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let command_line: CommandLine = argh::from_env();
    let realm_query =
        client::connect_to_protocol_at_path::<fsys2::RealmQueryMarker>(ROOT_REALM_QUERY)
            .expect("can connect to the root realm query");
    let provider = ArchiveAccessorProvider::new(realm_query);
    match command_line.execute(&provider).await {
        Ok(result) => {
            println!("{}", result);
        }
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }
    Ok(())
}
