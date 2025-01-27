// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_hang_args::HangCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx::TestingProxy;
use target_holders::daemon_protocol;

#[derive(FfxTool)]
pub struct DaemonHangTool {
    #[command]
    pub cmd: HangCommand,
    #[with(daemon_protocol())]
    testing_proxy: TestingProxy,
}

fho::embedded_plugin!(DaemonHangTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for DaemonHangTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> Result<()> {
        let _ = self.testing_proxy.hang().await;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::TestingRequest;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[fuchsia::test]
    async fn test_hang_with_no_text() {
        static HUNG: AtomicBool = AtomicBool::new(false);
        let proxy = fho::testing::fake_proxy(|req| match req {
            TestingRequest::Hang { .. } => {
                HUNG.store(true, Ordering::SeqCst);
            }
            _ => assert!(false),
        });
        let tool = DaemonHangTool { cmd: HangCommand {}, testing_proxy: proxy };
        let buffers = fho::TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);
        assert!(tool.main(writer).await.is_ok());
        assert!(HUNG.load(Ordering::SeqCst));
    }
}
