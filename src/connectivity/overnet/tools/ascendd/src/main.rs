// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {anyhow::Error, ascendd::Ascendd};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let hoist = hoist::Hoist::new(Some(std::time::Duration::from_millis(500)))?;
    Ascendd::new(argh::from_env(), &hoist).await?.await
}
