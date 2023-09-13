// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - list product bundle name to for specific SDK version.

use ::gcs::client::{Client, ProgressResponse};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_list_args::ListCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use pbms::string_from_url;
use pbms::AuthFlowChoice;
use serde::{Deserialize, Serialize};
use serde_json;
use std::io::Write;
use std::io::{stderr, stdin, stdout};
use structured_ui;

const PB_MANIFEST_NAME: &'static str = "product_bundles.json";
const CONFIG_BASE_URLS: &'static str = "pbms.base_urls";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ProductBundle {
    name: String,
    product_version: String,
    transfer_manifest_url: String,
}

type ProductManifest = Vec<ProductBundle>;

/// `ffx product list` sub-command.
#[ffx_plugin()]
pub async fn pb_list(
    cmd: ListCommand,
    #[ffx(machine = Vec<RepositoryConfig>)] mut writer: Writer,
) -> Result<()> {
    let mut input = stdin();
    let mut output = stdout();
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    if let Some(version) = cmd.version {
        let pbs = pb_list_impl(&cmd.auth, cmd.base_url, &version, &ui).await?;
        if writer.is_machine() {
            writer.machine(&pbs)?;
        } else {
            let pb_names = pbs.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
            let pb_string = pb_names.join("\n");
            writeln!(writer, "{}", pb_string)?;
        }
    } else {
        ffx_bail!("Currently we need the sdk version to exsit for the lookup")
    }

    Ok(())
}

pub async fn pb_list_impl<I>(
    auth: &AuthFlowChoice,
    override_base_url: Option<String>,
    version: &str,
    ui: &I,
) -> Result<Vec<ProductBundle>>
where
    I: structured_ui::Interface + Sync,
{
    let client = Client::initial()?;

    let mut products = Vec::new();
    let base_urls = if let Some(base_url) = override_base_url {
        vec![base_url]
    } else {
        ffx_config::get::<Vec<String>, _>(CONFIG_BASE_URLS)
            .await
            .context("get config CONFIG_BASE_URLS")?
            .iter()
            .map(|x| format!("{}/{}", x, version))
            .collect::<Vec<_>>()
    };

    for base_url in &base_urls {
        let prods = pb_gather_from_url(base_url, auth, ui, &client).await?;
        products.extend(prods);
    }

    let products =
        products.iter().cloned().filter(|x| x.product_version == version).collect::<Vec<_>>();
    Ok(products)
}

/// Fetch product bundle descriptions from a base URL.
async fn pb_gather_from_url<I>(
    base_url: &str,
    auth_flow: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<Vec<ProductBundle>>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("transfer_manifest_url Url::parse");
    let mut manifest_url = match url::Url::parse(&base_url) {
        Ok(p) => p,
        _ => ffx_bail!("The lookup location must be a URL, failed to parse {:?}", base_url),
    };
    gcs::gs_url::extend_url_path(&mut manifest_url, PB_MANIFEST_NAME)
        .with_context(|| format!("joining URL {:?} with file name", manifest_url))?;

    let pm = string_from_url(
        &manifest_url,
        auth_flow,
        &|state| {
            let mut progress = structured_ui::Progress::builder();
            progress.title("Getting product descriptions");
            progress.entry(&state.name, state.at, state.of, state.units);
            ui.present(&structured_ui::Presentation::Progress(progress))?;
            Ok(ProgressResponse::Continue)
        },
        ui,
        client,
    )
    .await
    .with_context(|| format!("string from gcs: {:?}", manifest_url))?;

    Ok(serde_json::from_str::<ProductManifest>(&pm)
        .with_context(|| format!("Parsing json {:?}", pm))?)
}

#[cfg(test)]
mod test {
    use {super::*, std::io::Write, temp_test_env::TempTestEnv};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_list_impl() {
        let test_env = TempTestEnv::new().expect("test_env");
        let mut f =
            std::fs::File::create(test_env.home.join(PB_MANIFEST_NAME)).expect("file create");
        f.write_all(
            r#"[{
            "name": "fake_name",
            "product_version": "fake_version",
            "transfer_manifest_url": "fake_url"
            }]"#
            .as_bytes(),
        )
        .expect("write_all");

        let ui = structured_ui::MockUi::new();
        let pbs = pb_list_impl(
            &AuthFlowChoice::Default,
            Some(format!("file:{}", test_env.home.display())),
            "fake_version",
            &ui,
        )
        .await
        .expect("testing list");
        assert_eq!(
            vec![ProductBundle {
                name: String::from("fake_name"),
                product_version: String::from("fake_version"),
                transfer_manifest_url: String::from("fake_url")
            }],
            pbs
        );
    }
}
