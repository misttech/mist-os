// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use ffx_tool_target_package::TargetPackageTool;
use fho::FfxTool as _;

#[fuchsia_async::run_singlethreaded]
async fn main() {
    TargetPackageTool::execute_tool().await
}
