// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_inspect::TreeRequestStream;
use fidl_fuchsia_inspect_deprecated::InspectRequestStream;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::component;
use futures::StreamExt;
use inspect_testing::serve_deprecated_inspect;
use inspect_testing::serve_inspect_tree;
use inspect_testing::ExampleInspectData;
use structopt::StructOpt;

enum IncomingServices {
    DeprecatedInspect(InspectRequestStream),
    InspectTree(TreeRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let opts = inspect_testing::Options::from_args();
    if opts.rows == 0 || opts.columns == 0 {
        inspect_testing::Options::clap().print_help()?;
        std::process::exit(1);
    }

    let mut inspect_data = ExampleInspectData::default();
    inspect_data.write_to(component::inspector().root());
    let node_object = inspect_data.get_node_object();

    let mut fs = ServiceFs::new();

    let vmo = component::inspector().duplicate_vmo().expect("failed to duplicate vmo");
    fs.dir("diagnostics").add_vmo_file_at("root.inspect", vmo);
    fs.dir("diagnostics").add_fidl_service(IncomingServices::DeprecatedInspect);
    fs.dir("diagnostics").add_fidl_service(IncomingServices::InspectTree);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, |service| async {
        match service {
            IncomingServices::DeprecatedInspect(s) => {
                serve_deprecated_inspect(s, node_object.clone()).await;
            }
            IncomingServices::InspectTree(s) => {
                serve_inspect_tree(s, component::inspector()).await;
            }
        }
    })
    .await;

    Ok(())
}
