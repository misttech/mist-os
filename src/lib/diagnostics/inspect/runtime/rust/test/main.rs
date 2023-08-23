// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::*;
use futures::prelude::*;
use inspect_runtime::PublishOptions;
use std::env::args;

#[fuchsia::main]
async fn main() {
    let root = component::inspector().root();
    root.record_int("int", 3);
    root.record_lazy_child("lazy-node", || {
        async move {
            let inspector = Inspector::default();
            inspector.root().record_string("a", "test");
            let child = inspector.root().create_child("child");
            child.record_lazy_values("lazy-values", || {
                async move {
                    let inspector = Inspector::default();
                    inspector.root().record_double("double", 3.25);
                    Ok(inspector)
                }
                .boxed()
            });
            inspector.root().record(child);
            Ok(inspector)
        }
        .boxed()
    });
    if args().nth(1) == Some("--use-diagnostics-dir".to_string()) {
        let mut fs = ServiceFs::new();
        inspect_runtime::serve(component::inspector(), &mut fs).unwrap();
        fs.take_and_serve_directory_handle().unwrap();

        fs.collect::<()>().await;
    } else {
        if let Some(inspect_server) = inspect_runtime::publish(
            component::inspector(),
            PublishOptions::default().inspect_tree_name("inspect_test_component"),
        ) {
            inspect_server.await;
        }
    }
}
