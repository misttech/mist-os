// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_e2e_emu::IsolatedEmulator;
use pretty_assertions::assert_eq;

#[fuchsia::test]
async fn echo_hello_world() -> Result<()> {
    let emu = IsolatedEmulator::start("echo-ssh").await?;
    let expected_message = "hello, world!";
    let message = emu.ssh_output(&["echo", expected_message]).await?;
    assert_eq!(message.trim(), expected_message);
    Ok(())
}
