// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_target_remove_args::RemoveCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter, ToolIO};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;

#[derive(FfxTool)]
pub struct RemoveTool {
    #[command]
    cmd: RemoveCommand,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(RemoveTool);

#[async_trait(?Send)]
impl FfxMain for RemoveTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        remove_impl(self.target_collection_proxy, self.cmd, writer).await?;
        Ok(())
    }
}

async fn remove_impl<W: ToolIO>(
    target_collection: TargetCollectionProxy,
    cmd: RemoveCommand,
    mut writer: W,
) -> Result<()> {
    let RemoveCommand { name_or_addr, .. } = cmd;
    if target_collection.remove_target(&name_or_addr).await? {
        writeln!(writer.stderr(), "Removed.")?;
    } else {
        writeln!(writer.stderr(), "No matching target found.")?;
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_writer::TestBuffers;
    use fidl_fuchsia_developer_ffx as ffx;

    fn setup_fake_target_collection_proxy<T: 'static + Fn(String) -> bool + Send>(
        test: T,
    ) -> TargetCollectionProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetCollectionRequest::RemoveTarget { target_id, responder } => {
                let result = test(target_id);
                responder.send(result).unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_remove_existing_target() {
        let server = setup_fake_target_collection_proxy(|id| {
            assert_eq!(id, "correct-horse-battery-staple".to_owned());
            true
        });
        let test_buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&test_buffers);
        remove_impl(
            server,
            RemoveCommand { name_or_addr: "correct-horse-battery-staple".to_owned() },
            writer,
        )
        .await
        .unwrap();

        assert_eq!(test_buffers.into_stderr_str(), "Removed.\n");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_remove_nonexisting_target() {
        let server = setup_fake_target_collection_proxy(|_| false);
        let test_buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&test_buffers);
        remove_impl(
            server,
            RemoveCommand { name_or_addr: "incorrect-donkey-battery-jazz".to_owned() },
            writer,
        )
        .await
        .unwrap();
        assert_eq!(test_buffers.into_stderr_str(), "No matching target found.\n");
    }
}
