// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::PathBuf;

use fuchsia_async as fasync;
use once_cell::sync::Lazy;
use tempfile::TempDir;

mod emu;

pub use emu::Emu;

pub struct TestContext {
    isolate: ffx_isolate::Isolate,
}

impl TestContext {
    pub fn isolate(&self) -> &ffx_isolate::Isolate {
        &self.isolate
    }
}

// Matches `gn` terminology (for host toolchain)
static ROOT_OUT_DIR: Lazy<PathBuf> = Lazy::new(|| {
    let mut dir = std::env::current_exe().expect("get path").canonicalize().unwrap();
    assert!(dir.pop());
    dir
});

// Matches `gn` terminology
static ROOT_BUILD_DIR: Lazy<PathBuf> = Lazy::new(|| {
    let mut dir = ROOT_OUT_DIR.clone();
    assert!(dir.pop());
    dir
});

// Matches `out_dir` var in `BUILD.gn`
static OUT_DIR: Lazy<PathBuf> = Lazy::new(|| ROOT_OUT_DIR.join("src/developer/ffx/testing"));

static TEMP_DIR: Lazy<TempDir> =
    Lazy::new(|| TempDir::new().expect("could not create test harness temp dir"));

/// Test fixture that handles launching and tearing down a test after execution.
pub async fn base_fixture<F, Fut>(case_name: &str, test_fn: F)
where
    F: FnOnce(TestContext) -> Fut + Send + 'static,
    Fut: futures::future::Future<Output = ()>,
{
    let test_env = ffx_config::test_init().await.expect("config init");

    // Not actually used. ffx will generate an ssh key for usage with the emulator.
    let ssh_path = OUT_DIR.join("ssh");

    let isolate = ffx_isolate::Isolate::new_in_test(case_name, ssh_path, &test_env.context)
        .await
        .expect("create isolate");
    let config = TestContext { isolate };

    // Spawn a new thread so that we can catch panics from the test. To avoid blocking this thread's
    // future executor, we check completion of the test thread using a oneshot channel.
    let (done_sender, done) = futures::channel::oneshot::channel();
    let join_handle = std::thread::spawn(move || {
        let mut test_executor = fasync::LocalExecutor::new();
        test_executor.run_singlethreaded(test_fn(config));
        // The sender will notify the receiver if it is used or dropped during a panic.
        let _ = done_sender.send(());
    });
    let _ = done.await;
    // After the receiver completes we know the test is done, so we can do a blocking join
    // without issue.
    let test_result = join_handle.join();

    // Propagate the panic from the test thread to fail the test, if needed.
    match test_result {
        Ok(()) => (),
        Err(test_err) => std::panic::resume_unwind(test_err),
    }
}
