// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - acquire and display product bundle information (metadata)
//! - acquire related data files, such as disk partition images (data)

use ::gcs::client::Client;
use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_product_download::pb_download_impl;
use ffx_product_get_args::GetCommand;
use ffx_product_lookup::pb_lookup_impl;
use std::{
    io::{stderr, stdin, stdout},
    path::Path,
};
use structured_ui;

/// `ffx product get` sub-command.
#[ffx_plugin("product.experimental")]
pub async fn pb_get(cmd: GetCommand) -> Result<()> {
    if !cmd.experimental {
        // The --experimental flag is intentionally not advertised to the user,
        // because the intent is for the user to call `download` instead.
        println!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        println!("@");
        println!("@  The `get` subcommand is renamed");
        println!("@");
        println!("@  Please use `ffx product download ...` instead.");
        println!("@");
        println!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
    }
    let mut input = stdin();
    let mut output = stdout();
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    let client = Client::initial()?;
    pb_get_impl(
        &cmd.auth,
        &cmd.context_uri,
        cmd.force,
        cmd.latest,
        &cmd.out_dir,
        &cmd.product_name,
        &cmd.version,
        &client,
        &ui,
    )
    .await
}

async fn pb_get_impl<I: structured_ui::Interface + Sync>(
    auth: &pbms::AuthFlowChoice,
    context_uri: &str,
    force: bool,
    look_in_latest: bool,
    out_dir: &Path,
    product_name: &Option<String>,
    product_version: &Option<String>,
    client: &Client,
    ui: &I,
) -> Result<()> {
    let start = std::time::Instant::now();
    tracing::info!("------------------ Begin get PBv2 -------------------");
    // There are three forms of input and each should resolve to a transfer url.
    let transfer_url = if context_uri.contains(":") && !context_uri.starts_with("pb:") {
        context_uri.to_string()
    } else {
        let (bucket, name, version) = if context_uri.starts_with("pb:") {
            let mut iter = context_uri.split(":");
            assert_eq!(iter.next(), Some("pb:"));
            let bucket = iter.next().expect("pb URI must have three colons").to_string();
            let name = iter.next().expect("pb URI must have three colons");
            let version = iter.next().expect("pb URI must have three colons");
            assert_eq!(iter.next(), None);
            (bucket, name, version)
        } else {
            let name = product_name
                .as_ref()
                .expect("when not passing a URI, a <name> is required.")
                .as_str();
            let version = product_version.as_ref().unwrap().as_str();
            (context_uri.to_string(), name, version)
        };

        let version = if look_in_latest {
            #[cfg(target_os = "linux")]
            let latest_url = url::Url::parse(&format!("gs://{}/development/LATEST_LINUX", bucket))?;
            #[cfg(target_os = "macos")]
            let latest_url = url::Url::parse(&format!("gs://{}/development/LATEST_MAC", bucket))?;
            let version = pbms::string_from_url(
                &latest_url,
                auth,
                &|state| {
                    let mut progress = structured_ui::Progress::builder();
                    progress.title("Getting product latest version string");
                    progress.entry(&state.name, state.at, state.of, state.units);
                    ui.present(&structured_ui::Presentation::Progress(progress))?;
                    Ok(gcs::client::ProgressResponse::Continue)
                },
                ui,
                client,
            )
            .await
            .context("getting latest version string.")?;
            let version = version.trim();
            assert!(version.len() > 0, "latest version string is empty!");
            version.to_string()
        } else {
            version.to_string()
        };

        tracing::debug!("pb_lookup_impl {} {} {}", bucket, name, version);
        println!("pb_lookup_impl {} {} {}", bucket, name, version);
        pb_lookup_impl(
            &auth,
            &format!("gs://{}/development/{}", bucket, version),
            name,
            &version,
            ui,
        )
        .await?
    };

    println!("pb_download_impl {}", transfer_url);
    pb_download_impl(&auth, force, &transfer_url, out_dir, &client, ui).await?;

    tracing::debug!("Total ffx product get runtime {} seconds.", start.elapsed().as_secs_f32());
    tracing::debug!("End");
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*,
        fuchsia_hyper_test_support::{
            handler::{ForPath, StaticResponse},
            TestServer,
        },
        tempfile,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_get_impl() {
        let test_dir = tempfile::TempDir::new().expect("temp dir");
        let server = TestServer::builder()
            .handler(ForPath::new(
                "/example/fake/transfer.json",
                StaticResponse::ok_body(
                    r#"
            {
                "version": "1",
                "entries": [{
                    "type": "files",
                    "local": "foo",
                    "remote": "data",
                    "entries": [{ "name": "payload.txt"}]
                }]
            }"#,
                ),
            ))
            .handler(ForPath::new("/api/b/example/o", StaticResponse::ok_body(r#"{}"#)))
            .start()
            .await;
        let auth = pbms::AuthFlowChoice::Default;
        let force = false;
        let latest = false;
        let manifest_url = "gs://example/fake/transfer.json".to_string();
        let out_dir = test_dir.path().join("download");
        let client = Client::initial_with_urls(
            &server.local_url_for_path("api"),
            &server.local_url_for_path("storage"),
        )
        .expect("creating client");
        let ui = structured_ui::MockUi::new();
        pb_get_impl(&auth, &manifest_url, force, latest, &out_dir, &None, &None, &client, &ui)
            .await
            .expect("testing download");
    }
}
