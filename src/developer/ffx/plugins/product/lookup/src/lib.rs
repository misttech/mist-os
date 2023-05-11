// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - lookup product bundle description information to find the transfer URL.

use ::gcs::client::{Client, ProgressResponse};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_lookup_args::LookupCommand;
use pbms::string_from_url;
use pbms::AuthFlowChoice;
use serde::{Deserialize, Serialize};
use serde_json;
use std::io::{stderr, stdin, stdout};
use structured_ui;

const PB_MANIFEST_NAME: &'static str = "product_bundles.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProductBundle {
    name: String,
    product_version: String,
    transfer_manifest_url: String,
}

type ProductManifest = Vec<ProductBundle>;

/// `ffx product lookup` sub-command.
#[ffx_plugin()]
pub async fn pb_lookup(cmd: LookupCommand) -> Result<()> {
    let mut input = stdin();
    let mut output = stdout();
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    pb_lookup_impl(&cmd.auth, &cmd.base_url, &cmd.name, &cmd.version, &ui).await?;
    Ok(())
}

pub async fn pb_lookup_impl<I>(
    auth: &AuthFlowChoice,
    base_url: &str,
    name: &str,
    version: &str,
    ui: &I,
) -> Result<String>
where
    I: structured_ui::Interface + Sync,
{
    let start = std::time::Instant::now();
    tracing::info!("---------------------- Lookup Begin ----------------------------");
    let client = Client::initial()?;
    let products = pb_gather_from_url(base_url, auth, ui, &client).await?;

    tracing::debug!("Looking for product bundle {}, version {}", name, version);
    let products = products
        .iter()
        .filter(|x| x.name == name)
        .filter(|x| x.product_version == version)
        .map(|x| x.to_owned())
        .collect::<Vec<ProductBundle>>();
    if products.is_empty() {
        tracing::debug!("products {:?}", products);
        println!("Error: No product matching name {}, version {} found.", name, version);
        std::process::exit(1);
    } else if products.len() > 1 {
        tracing::debug!("products {:?}", products);
        println!("More than one matching product found. The base-url may have poorly formed data.");
        std::process::exit(1);
    } else {
        println!("{}", products[0].transfer_manifest_url.to_owned());
    }
    tracing::debug!("Total ffx product lookup runtime {} seconds.", start.elapsed().as_secs_f32());
    tracing::debug!("End");
    Ok(products[0].transfer_manifest_url.to_owned())
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
    async fn test_pb_lookup_impl() {
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
        let url = pb_lookup_impl(
            &AuthFlowChoice::Default,
            &format!("file:{}", test_env.home.display()),
            "fake_name",
            "fake_version",
            &ui,
        )
        .await
        .expect("testing lookup");
        assert_eq!(url, "fake_url");
    }
}
