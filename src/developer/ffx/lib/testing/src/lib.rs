// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use {
    fidl_fuchsia_developer_remotecontrol as fremotecontrol,
    fidl_fuchsia_test_manager as ftest_manager,
};

const RUN_BUILDER_MONIKER: &str = "/core/test_manager";
const QUERY_MONIKER: &str = "/core/test_manager";
const EARLY_BOOT_PROFILE_MONIKER: &str = "/core/test_manager";

/// Timeout for connecting to test manager. This is a longer timeout than the timeout given for
/// connecting to other protocols, as during the first run
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Connect to `fuchsia.test.manager.RunBuilder` on a target device using an RCS connection.
pub async fn connect_to_run_builder(
    remote_control: &fremotecontrol::RemoteControlProxy,
) -> Result<ftest_manager::RunBuilderProxy> {
    rcs::connect_to_protocol::<ftest_manager::RunBuilderMarker>(
        TIMEOUT,
        RUN_BUILDER_MONIKER,
        remote_control,
    )
    .await
}

/// Connect to `fuchsia.test.manager.Query` on a target device using an RCS connection.
pub async fn connect_to_query(
    remote_control: &fremotecontrol::RemoteControlProxy,
) -> Result<ftest_manager::QueryProxy> {
    rcs::connect_to_protocol::<ftest_manager::QueryMarker>(TIMEOUT, QUERY_MONIKER, remote_control)
        .await
}

/// Connect to `fuchsia.test.manager.EarlyBootProfile` on a target device using an RCS connection.
pub async fn connect_to_early_boot_profile(
    remote_control: &fremotecontrol::RemoteControlProxy,
) -> Result<ftest_manager::EarlyBootProfileProxy> {
    rcs::connect_to_protocol::<ftest_manager::EarlyBootProfileMarker>(
        TIMEOUT,
        EARLY_BOOT_PROFILE_MONIKER,
        remote_control,
    )
    .await
}
