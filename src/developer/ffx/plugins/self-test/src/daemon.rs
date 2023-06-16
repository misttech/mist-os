// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{assert_eq, test::new_isolate};
use anyhow::*;
use fuchsia_async::Duration;

pub(crate) async fn test_echo() -> Result<()> {
    let isolate = new_isolate("daemon-echo").await?;
    isolate.start_daemon().await?;
    let out = isolate.ffx(&["daemon", "echo"]).await?;

    let want = "SUCCESS: received \"Ffx\"\n";
    assert_eq!(out.stdout, want);

    Ok(())
}

pub(crate) async fn test_config_flag() -> Result<()> {
    let isolate = new_isolate("daemon-config-flag").await?;
    let mut daemon = isolate.start_daemon().await?;

    assert_eq!(None, daemon.try_wait()?, "Daemon exited quickly after starting");

    // This should not terminate the daemon just started, as it won't
    // share an overnet socket with it.
    let mut ascendd_path2 = isolate.ascendd_path().clone();
    ascendd_path2.set_extension("2");
    let _out = isolate
        .ffx(&[
            "--config",
            &format!("overnet.socket={}", ascendd_path2.to_string_lossy()),
            "daemon",
            "stop",
            "-t",
            "1000",
        ])
        .await?;

    assert_eq!(
        None,
        daemon.try_wait()?,
        "Daemon didn't stay up after the stop message was sent to the other socket."
    );

    // TODO(): on the mac, we may need to explicitly tell the daemon to exit,
    // because the socket-watcher doesn't seem to always work.  We don't want
    // to use "ffx daemon stop" in general, however, since the new daemon may
    // not yet have created the ascendd socket. (This usually happens only with
    // targets, so since we don't do host tests on the mac with targets, this
    // should be a non-issue on mac builders, at least for now.)
    if cfg!(target_os = "macos") {
        fuchsia_async::Timer::new(Duration::from_millis(500)).await;
        let _ = isolate.ffx(&["daemon", "stop", "-t", "3000"]).await?;
    }

    // Because we created the daemon, it won't go away (i.e. in cleanup_isolate())
    // until we wait() for it. So instead we drop the isolate here, then wait.
    drop(isolate);
    fuchsia_async::unblock(move || daemon.wait()).await?;
    Ok(())
}

pub(crate) async fn test_stop() -> Result<()> {
    let isolate = new_isolate("daemon-stop").await?;
    let out = isolate.ffx(&["daemon", "stop", "-t", "3000"]).await?;
    let want = "No daemon was running.\n";
    assert_eq!(out.stdout, want);

    let daemon = isolate.start_daemon().await?;

    // Reap the daemon in another thread so the daemon exits.
    // Otherwise the process will still exist but in a zombie state.
    let _ = std::thread::spawn(move || { daemon }.wait());

    let out = isolate.ffx(&["daemon", "stop", "-t", "3000"]).await?;
    let want = "Stopped daemon.\n";
    assert_eq!(out.stdout, want);

    Ok(())
}

pub(crate) async fn test_no_autostart() -> Result<()> {
    let isolate = new_isolate("daemon-no-autostart").await?;
    let out = isolate.ffx(&["daemon", "echo"]).await?;
    assert!(!out.status.success());
    assert!(out.stderr.contains(
        "FFX Daemon was told not to autostart and no existing Daemon instance was found"
    ));

    let mut daemon = isolate.start_daemon().await?;

    assert_eq!(None, daemon.try_wait()?, "Daemon exited quickly after starting");

    let out = isolate.ffx(&["daemon", "echo"]).await?;
    // Don't assert here -- it if fails, we still want to kill the daemon
    let echo_succeeded = out.status.success();

    // Want to kill the daemon here, rather than in cleanup_isolate(), because since we
    // forked it explicitly, we want to wait()
    let _ = daemon.kill();
    daemon.wait().expect("Daemon wasn't running");

    assert!(echo_succeeded);

    Ok(())
}
