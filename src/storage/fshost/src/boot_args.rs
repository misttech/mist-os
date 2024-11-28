// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_boot::{ArgumentsMarker, BoolPair};
use fuchsia_component::client::connect_to_protocol;

#[derive(Default)]
pub struct BootArgs {
    netsvc_netboot: bool,
}

impl BootArgs {
    pub async fn new() -> Self {
        Self::new_helper().await.unwrap_or_else(|_| BootArgs::default())
    }

    pub async fn new_helper() -> Result<Self, Error> {
        let arguments_proxy = connect_to_protocol::<ArgumentsMarker>()
            .context("Failed to connect to Arguments protocol")?;

        let defaults = &[BoolPair { key: "netsvc.netboot".to_string(), defaultval: false }];
        let ret = arguments_proxy.get_bools(defaults).await.context("get_bools failed")?;

        let netsvc_netboot = ret[0];

        Ok(BootArgs { netsvc_netboot })
    }

    pub fn netboot(&self) -> bool {
        self.netsvc_netboot
    }
}
