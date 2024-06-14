// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use run_test_suite_lib::{ConnectionError, RunBuilderConnector};
use {
    fidl_fuchsia_developer_remotecontrol as fremotecontrol,
    fidl_fuchsia_test_manager as ftest_manager,
};

/// Connector implementation that connects to RunBuilder using the RemoteControl
/// protocol.
pub(crate) struct RunConnector {
    remote_control_proxy: fremotecontrol::RemoteControlProxy,
    batch_size: usize,
}

impl RunConnector {
    pub fn new(
        remote_control_proxy: fremotecontrol::RemoteControlProxy,
        batch_size: usize,
    ) -> Self {
        Self { remote_control_proxy, batch_size }
    }
}

#[async_trait::async_trait]
impl RunBuilderConnector for RunConnector {
    async fn connect(&self) -> Result<ftest_manager::RunBuilderProxy, ConnectionError> {
        testing_lib::connect_to_run_builder(&self.remote_control_proxy)
            .await
            .map_err(ConnectionError)
    }

    fn batch_size(&self) -> usize {
        self.batch_size
    }
}
