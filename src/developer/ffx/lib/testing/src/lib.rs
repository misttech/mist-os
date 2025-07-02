// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use {
    fidl_fuchsia_developer_remotecontrol as fremotecontrol,
    fidl_fuchsia_test_manager as ftest_manager,
};

const SUITE_RUNNER_MONIKER: &str = "/core/test_manager";
const TEST_CASE_ENUMERATOR_MONIKER: &str = "/core/test_manager";
const EARLY_BOOT_PROFILE_MONIKER: &str = "/core/test_manager";

/// Timeout for connecting to test manager. This is a longer timeout than the timeout given for
/// connecting to other protocols, as during the first run
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Connect to `fuchsia.test.manager.SuiteRunner` on a target device using an RCS connection.
pub async fn connect_to_suite_runner(
    remote_control: &fremotecontrol::RemoteControlProxy,
) -> Result<ftest_manager::SuiteRunnerProxy> {
    rcs::connect_to_protocol::<ftest_manager::SuiteRunnerMarker>(
        TIMEOUT,
        SUITE_RUNNER_MONIKER,
        remote_control,
    )
    .await
}

/// Connect to `fuchsia.test.manager.TestCaseEnumerator` on a target device using an RCS connection.
pub async fn connect_to_test_case_enumerator(
    remote_control: &fremotecontrol::RemoteControlProxy,
) -> Result<ftest_manager::TestCaseEnumeratorProxy> {
    rcs::connect_to_protocol::<ftest_manager::TestCaseEnumeratorMarker>(
        TIMEOUT,
        TEST_CASE_ENUMERATOR_MONIKER,
        remote_control,
    )
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
