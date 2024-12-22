// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod agis_lib;
mod args;

use agis_lib::AgisTool;
use fho::FfxTool;

#[fuchsia_async::run_singlethreaded]
async fn main() {
    AgisTool::execute_tool().await
}
