// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use anyhow::{format_err, Result};
use args::DisableCommand;
use fidl_fuchsia_driver_development as fdd;
use std::io::Write;
use zx_status::Status;

pub async fn disable(
    cmd: DisableCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::ManagerProxy,
) -> Result<()> {
    writeln!(writer, "Disabling {} and restarting driver hosts with rematching enabled.", cmd.url)?;

    let result = driver_development_proxy.disable_driver(&cmd.url, None).await?;
    match result {
        Ok(_) => {
            writeln!(writer, "Disabled driver successfully.")?;
        }
        Err(e) => {
            if e == Status::NOT_FOUND.into_raw() {
                writeln!(writer, "No drivers affected in this disable operation.")?;
            } else {
                writeln!(writer, "Unexpected error from disable: {}", e)?;
            }
        }
    }

    let restart_result = driver_development_proxy
        .restart_driver_hosts(
            cmd.url.as_str(),
            fdd::RestartRematchFlags::REQUESTED | fdd::RestartRematchFlags::COMPOSITE_SPEC,
        )
        .await?;

    match restart_result {
        Ok(count) => {
            if count > 0 {
                writeln!(
                    writer,
                    "Successfully restarted and rematching {} driver hosts with the driver.",
                    count
                )?;
            } else {
                writeln!(writer, "{}", "There are no existing driver hosts with this driver.")?;
            }
        }
        Err(err) => {
            return Err(format_err!(
                "Failed to restart existing drivers: {:?}",
                Status::from_raw(err)
            ));
        }
    }

    Ok(())
}
