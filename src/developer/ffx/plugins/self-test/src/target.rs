// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test::*;
use anyhow::*;
use ffx_executor::FfxExecutor;
use std::time::Duration;

pub(crate) async fn test_manual_add_get_ssh_address() -> Result<()> {
    let isolate = new_isolate("target-manual-add-get-ssh-address").await?;
    isolate.start_daemon().await?;

    let _ = isolate.exec_ffx(&["target", "add", "--nowait", "[::1]:8022"]).await?;

    let out = isolate.exec_ffx(&["--target", "[::1]:8022", "target", "get-ssh-address"]).await?;

    ensure!(out.stdout.contains("[::1]:8022"), "stdout is unexpected: {:?}", out);
    ensure!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);
    // TODO: establish a good way to assert against the whole target address.

    Ok(())
}

pub(crate) async fn test_manual_add_get_ssh_address_late_add() -> Result<()> {
    let isolate = new_isolate("target-manual-add-get-ssh-address-late-add").await?;
    isolate.start_daemon().await?;

    let task =
        isolate.exec_ffx(&["--target", "[::1]:8022", "target", "get-ssh-address", "-t", "10"]);

    // The get-ssh-address should pick up targets added after it has started, as well as before.
    fuchsia_async::Timer::new(Duration::from_millis(500)).await;

    let _ = isolate.exec_ffx(&["target", "add", "--nowait", "[::1]:8022"]).await?;

    let out = task.await?;

    ensure!(out.stdout.contains("[::1]:8022"), "stdout is unexpected: {:?}", out);
    ensure!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);
    // TODO: establish a good way to assert against the whole target address.

    Ok(())
}

pub mod include_target {
    use super::*;

    // TODO(slgrady): Create tests for "ffx target list" (currently non-existent
    // since the previous test was dependent on discovery), and getting a target
    // by name (currently non-existent since these tests only expect to have an
    // address specified)

    // Check that the addresses match, and that the output includes the port.
    // We'd like to write a test that validates the port that comes back from
    // get-ssh-address is the _correct_ port, but we can't actually write that,
    // because we don't necessarily know the port. The "target addr" could
    // simply be specified as an IP address address _without a port_ (e.g. a
    // target of "192.168.42.105"). In that situation, the address is used as a
    // _matching query_, trying to find the target with the specified address.
    // So we get back the port, but we don't know what to compare it to.
    pub(crate) async fn test_get_ssh_address_includes_port() -> Result<()> {
        let isolate = new_isolate("target-get-ssh-address-includes-port").await?;
        isolate.start_daemon().await?;

        let ta = get_target_addr();
        let target_nodeaddr = ta.trim();

        let addr = {
            let (addr, _scope, _port) = netext::parse_address_parts(target_nodeaddr)
                .context(format!("Couldn't parse nodeaddr {:?} as addr", target_nodeaddr))?;
            addr
        };

        let out = isolate
            .exec_ffx(&["--target", &target_nodeaddr, "target", "get-ssh-address", "-t", "5"])
            .await?;

        let out_addr = out.stdout.trim();
        let (out_addr, out_port) = {
            let (addr, _scope, port) = netext::parse_address_parts(out_addr)
                .context(format!("Couldn't parse out_addr {:?} as addr", out_addr))?;
            (addr, port)
        };
        ensure!(addr == out_addr, "expected stdout to use addr of {addr}, got {:?}", out);
        ensure!(out_port.is_some(), "expected stdout to provide port, got {:?}", out);
        ensure!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);
        // TODO: establish a good way to assert against the whole target address.

        Ok(())
    }

    pub(crate) async fn test_target_show() -> Result<()> {
        let isolate = new_isolate("target-show").await?;
        isolate.start_daemon().await?;

        let target_nodeaddr = get_target_addr();

        let out = isolate.exec_ffx(&["--target", &target_nodeaddr, "target", "show"]).await?;

        ensure!(out.status.success(), "status is unexpected: {:?}", out);
        ensure!(!out.stdout.is_empty(), "stdout is unexpectedly empty: {:?}", out);
        ensure!(out.stderr.lines().count() == 0, "stderr is unexpected: {:?}", out);

        Ok(())
    }
}
