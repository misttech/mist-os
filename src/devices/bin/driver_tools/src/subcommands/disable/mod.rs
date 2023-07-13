// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    anyhow::{format_err, Result},
    args::DisableCommand,
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_pkg as fpkg,
    fuchsia_zircon_status::Status,
    std::io::Write,
};

pub async fn disable(
    cmd: DisableCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::DriverDevelopmentProxy,
) -> Result<()> {
    writeln!(writer, "Disabling {} and restarting driver hosts with rematching enabled.", cmd.url)?;

    driver_development_proxy
        .disable_match_with_driver_url(&fpkg::PackageUrl { url: cmd.url.to_string() })
        .await?;

    let restart_result = driver_development_proxy
        .restart_driver_hosts(cmd.url.as_str(), fdd::RematchFlags::REQUESTED)
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
                writeln!(writer, "{}", "There are no existing driver hosts with this driver.",)?;
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
