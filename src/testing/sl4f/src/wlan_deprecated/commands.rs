// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::server::Facade;
use crate::wlan_deprecated::facade::WlanDeprecatedConfigurationFacade;
use anyhow::{format_err, Error};
use async_trait::async_trait;
use log::info;
use serde_json::{to_value, Value};

#[async_trait(?Send)]
impl Facade for WlanDeprecatedConfigurationFacade {
    async fn handle_request(&self, method: String, args: Value) -> Result<Value, Error> {
        match method.as_ref() {
            "suggest_ap_mac" => {
                let mac = self.parse_mac_argument(args)?;
                info!(
                    tag = "WlanDeprecatedConfigurationFacade";
                    "setting suggested MAC to: {:?}", mac
                );
                let result = self.suggest_access_point_mac_address(mac).await?;
                to_value(result).map_err(|e| {
                    format_err!("error parsing suggested access point MAC result: {}", e)
                })
            }
            _ => return Err(format_err!("unsupported command!")),
        }
    }
}
