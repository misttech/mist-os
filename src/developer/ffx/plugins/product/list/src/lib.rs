// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - list product bundle name to for specific SDK version.

use ::gcs::client::{Client, ProgressResponse};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_config::sdk::SdkVersion;
use ffx_core::ffx_plugin;
use ffx_product_list_args::ListCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use pbms::string_from_url;
use pbms::AuthFlowChoice;
use serde::{Deserialize, Serialize};
use serde_json;
use std::io::{stderr, stdin, stdout, Write};
use structured_ui;

const PB_MANIFEST_NAME: &'static str = "product_bundles.json";
const CONFIG_BASE_URLS: &'static str = "pbms.base_urls";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ProductBundle {
    pub name: String,
    pub product_version: String,
    pub transfer_manifest_url: String,
}

type ProductManifest = Vec<ProductBundle>;

/// `ffx product list` sub-command.
#[ffx_plugin()]
pub async fn pb_list(
    cmd: ListCommand,
    #[ffx(machine = Vec<RepositoryConfig>)] mut writer: Writer,
) -> Result<()> {
    let mut input = stdin();
    // Emit machine progress info to stderr so users can redirect it to /dev/null.
    let mut output = if writer.is_machine() {
        Box::new(stderr()) as Box<dyn Write + Send + Sync>
    } else {
        Box::new(stdout())
    };
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    let version = match cmd.version {
        Some(version) => version,
        None => {
            let sdk = ffx_config::global_env_context()
                .context("loading global environment context")?
                .get_sdk()
                .await
                .context("getting sdk env context")?;
            match sdk.get_version() {
                SdkVersion::Version(version) => version.to_string(),
                SdkVersion::InTree => {
                    ffx_bail!("Using in-tree sdk. Please specify the version through '--version'")
                }
                SdkVersion::Unknown => ffx_bail!("Unable to determine SDK version"),
            }
        }
    };

    let pbs = pb_list_impl(&cmd.auth, cmd.base_url, &version, &ui).await?;
    if writer.is_machine() {
        writer.machine(&pbs)?;
    } else {
        let pb_names = pbs.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        let pb_string = pb_names.join("\n");
        writeln!(writer, "{}", pb_string)?;
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
        let prods = pb_gather_from_url(base_url, auth, ui, &client).await.unwrap_or_else(|_| {
            println!("Failed to fetch from base_url: {}", &base_url);
            Vec::new()
        });
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
    use {super::*, ffx_writer::Format, std::io::Write, temp_test_env::TempTestEnv};

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

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_list_impl_machine_code() {
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

        let writer = Writer::new_test(Some(Format::Json));
        let () = pb_list(
            ListCommand {
                auth: AuthFlowChoice::Default,
                base_url: Some(format!("file:{}", test_env.home.display())),
                version: Some("fake_version".into()),
            },
            writer.clone(),
        )
        .await
        .expect("testing list");

        let pbs: Vec<ProductBundle> = serde_json::from_str(&writer.test_output().unwrap()).unwrap();
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
