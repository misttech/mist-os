// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test::*;
use anyhow::*;
use ffx_executor::FfxExecutor;

pub mod include_target {
    use super::*;

    pub(crate) async fn test_list() -> Result<()> {
        let isolate = new_isolate("component-list").await?;
        isolate.start_daemon().await?;
        let target_nodeaddr = get_target_addr();

        let out = isolate.exec_ffx(&["--target", &target_nodeaddr, "component", "list"]).await?;

        ensure!(out.status.success(), "status is unexpected: {:?}", out);
        ensure!(!out.stdout.is_empty(), "stdout is unexpectedly empty: {:?}", out);
        ensure!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);

        Ok(())
    }
}
