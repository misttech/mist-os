// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use stressor_lib::{aggressive::Stressor as Aggressive, gentle::Stressor as Gentle};

#[fuchsia::main]
fn main() {
    // Give the system some time to start.
    std::thread::sleep(std::time::Duration::from_secs(10));

    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--mode=aggressive".to_owned()) {
        tracing::info!("Aggressive mode.");
        Aggressive::new().run(8);
    } else {
        tracing::info!("Gentle mode.");
        Gentle::new().run(8);
    }
}
