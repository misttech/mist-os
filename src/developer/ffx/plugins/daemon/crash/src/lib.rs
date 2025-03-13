// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_crash_args::CrashCommand;
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool, Result};
use fidl_fuchsia_developer_ffx::TestingProxy;
use target_holders::daemon_protocol;
#[derive(FfxTool)]
pub struct DaemonCrashTool {
    #[command]
    pub cmd: CrashCommand,
    #[with(daemon_protocol())]
    testing_proxy: TestingProxy,
}

fho::embedded_plugin!(DaemonCrashTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for DaemonCrashTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> Result<()> {
        let _ = self.testing_proxy.crash().await;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::TestingRequest;
    use std::sync::atomic::{AtomicBool, Ordering};
    use target_holders::fake_proxy;

    #[fuchsia::test]
    async fn test_crash_with_no_text() {
        // XXX(raggi): if we can bound the lifetime of the testing proxy setup as
        // desired by the test, then we could avoid the need for the static.
        static CRASHED: AtomicBool = AtomicBool::new(false);
        let proxy = fake_proxy(|req| match req {
            TestingRequest::Crash { .. } => {
                CRASHED.store(true, Ordering::SeqCst);
            }
            _ => assert!(false),
        });
        let tool = DaemonCrashTool { cmd: CrashCommand {}, testing_proxy: proxy };
        let buffers = ffx_writer::TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);
        assert!(tool.main(writer).await.is_ok());
        assert!(CRASHED.load(Ordering::SeqCst));
    }
}
