// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - acquire and display product bundle information (metadata)
//! - acquire related data files, such as disk partition images (data)

use ::gcs::client::{Client, ProgressResponse, ProgressState};
use anyhow::{anyhow, Context, Result};
use async_fs::rename;
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_get_args::GetCommand;
use pbms::{make_way_for_output, transfer_download};
use std::io::{stderr, stdin, stdout};
use structured_ui;

/// `ffx product get` sub-command.
#[ffx_plugin("product.experimental")]
pub async fn pb_get(cmd: GetCommand) -> Result<()> {
    let mut input = stdin();
    let mut output = stdout();
    let mut err_out = stderr();
    let mut ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    // TODO(fxbug.dev/118921): Do not put a .context() on this because it will
    // break ffx_bail!().
    pb_get_impl(&cmd, &mut ui).await
}

async fn pb_get_impl<I: structured_ui::Interface + Sync>(
    cmd: &GetCommand,
    ui: &mut I,
) -> Result<()> {
    let start = std::time::Instant::now();
    tracing::info!("---------------------- Begin ----------------------------");
    tracing::debug!("transfer_manifest_url Url::parse");
    let transfer_manifest_url = match url::Url::parse(&cmd.manifest_url) {
        Ok(p) => p,
        _ => ffx_bail!("The source location must be a URL, failed to parse {:?}", cmd.manifest_url),
    };
    let local_dir = &cmd.out_dir;
    tracing::debug!("make_way_for_output {:?}", local_dir);
    // TODO(fxbug.dev/118921): Do not put a .context() on this because it will
    // break ffx_bail!().
    make_way_for_output(&local_dir, cmd.force).await?;

    let parent_dir = local_dir.parent().ok_or_else(|| anyhow!("local dir has no parent"))?;
    let temp_dir = tempfile::TempDir::new_in(&parent_dir)?;
    let client = Client::initial()?;
    tracing::debug!("transfer_manifest, transfer_manifest_url {:?}", transfer_manifest_url);
    transfer_download(
        &transfer_manifest_url,
        &temp_dir.path(),
        &cmd.auth,
        &|layers| {
            let mut progress = structured_ui::Progress::builder();
            progress.title("Transfer download");
            progress.entry("Transfer manifest", /*at=*/ 1, /*of=*/ 2, "steps");
            for layer in layers {
                progress.entry(&layer.name, layer.at, layer.of, layer.units);
            }
            ui.present(&structured_ui::Presentation::Progress(progress))?;
            Ok(ProgressResponse::Continue)
        },
        ui,
        &client,
    )
    .await
    .context("downloading via transfer manifest")?;

    // Workaround for having the product bundle nested in a sub-dir.
    let extra_dir = temp_dir.path().join("product_bundle");
    let pb_dir = if extra_dir.exists() { extra_dir } else { temp_dir.path().to_path_buf() };
    rename(&pb_dir, &local_dir)
        .await
        .with_context(|| format!("moving dir {:?} to {:?}", pb_dir, local_dir))?;

    let layers = vec![ProgressState { name: "complete", at: 2, of: 2, units: "steps" }];
    let mut progress = structured_ui::Progress::builder();
    progress.title("Transfer download");
    for layer in layers {
        progress.entry(&layer.name, layer.at, layer.of, layer.units);
    }
    ui.present(&structured_ui::Presentation::Progress(progress))?;

    tracing::debug!(
        "Total fx product-bundle get runtime {} seconds.",
        start.elapsed().as_secs_f32()
    );
    tracing::debug!("End");
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile;

    #[ignore]
    #[should_panic(expected = "downloading via transfer manifest")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_get_impl() {
        let test_dir = tempfile::TempDir::new().expect("temp dir");
        let cmd = GetCommand {
            auth: pbms::AuthFlowChoice::Default,
            force: false,
            out_dir: test_dir.path().to_path_buf(),
            manifest_url: "gs://example/fake/transfer.json".to_string(),
        };
        let mut ui = structured_ui::MockUi::new();
        pb_get_impl(&cmd, &mut ui).await.expect("testing get");
    }
}
