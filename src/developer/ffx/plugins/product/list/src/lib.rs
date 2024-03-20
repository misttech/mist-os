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
use gcs::gs_url::split_gs_url;
use lazy_static::lazy_static;
use maplit::hashmap;
use omaha_client::version::Version;
use pbms::AuthFlowChoice;
use pbms::{list_from_gcs, string_from_url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{stderr, stdin, stdout, Write};
use std::str::FromStr;

const PB_MANIFEST_NAME: &'static str = "product_bundles.json";
const CONFIG_BASE_URLS: &'static str = "pbms.base_urls";

lazy_static! {
    static ref BRANCH_TO_PREFIX_MAPPING: HashMap<&'static str, &'static str> = hashmap! {
    "f12" => "12.20230611.1",
    "f13" => "13.20230724.3",
    "f14" => "14.202308",
    "f15" => "15.20231018.3",
    "f16" => "16.20231130.3",
    "f17" => "17.20240122.0",
    "f18" => "18.20240225.3"};
}

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

    let pbs = pb_list_impl(&cmd.auth, cmd.base_url, cmd.version, cmd.branch, &ui).await?;
    if writer.is_machine() {
        writer.machine(&pbs)?;
    } else {
        let pb_names = pbs.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        let pb_string = pb_names.join("\n");
        writeln!(writer, "{}", pb_string)?;
    }

    Ok(())
}

pub async fn resolve_branch_to_base_urls<I>(
    version: Option<String>,
    branch: Option<String>,
    auth: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<Vec<String>>
where
    I: structured_ui::Interface,
{
    let base_urls = ffx_config::get::<Vec<String>, _>(CONFIG_BASE_URLS)
        .await
        .context("get config CONFIG_BASE_URLS")?;

    // If branch is not provided, use version to build base_urls
    if branch.is_none() {
        let version = match version {
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
                        ffx_bail!(
                            "Using in-tree sdk. Please specify the version through '--version'"
                        )
                    }
                    SdkVersion::Unknown => {
                        ffx_bail!(
                            "Unable to determine SDK version. Please specify the version through '--version'")
                    }
                }
            }
        };
        return Ok(base_urls.iter().map(|x| format!("{}/{}", x, version)).collect::<Vec<_>>());
    }

    // If branch is not none, resolve it to latest available version.

    // Error out if both version and branch are provided.
    if version.is_some() {
        ffx_bail!("Cannot provide version and branch at the same time");
    }

    let branch = branch.unwrap();
    let mut result = Vec::new();
    for base_url in base_urls {
        let (bucket, _) = split_gs_url(&base_url).context("Splitting gs URL.")?;
        let prefix = format!(
            "development/{}",
            BRANCH_TO_PREFIX_MAPPING.get(branch.as_str()).expect("Branch value is not supported!")
        );
        let version = get_latest_version(bucket, &prefix, auth, ui, &client).await?;
        result.push(format!("{}/{}", base_url, version));
    }

    Ok(result)
}

pub async fn get_latest_version<I>(
    bucket: &str,
    prefix: &str,
    auth: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<String>
where
    I: structured_ui::Interface,
{
    let list = list_from_gcs(bucket, prefix, auth, ui, &client)
        .await
        .with_context(|| "Listing the objects")?;
    let mut filtered_list = list
        .iter()
        .filter(|x| x.contains("product_bundles.json"))
        .map(|x| {
            let v = x
                .trim_start_matches("development/")
                .trim_end_matches("/product_bundles.json")
                .trim_end_matches("/sdk");
            Version::from_str(&v).expect("version cannot be parsed")
        })
        .collect::<Vec<_>>();
    filtered_list.sort();
    Ok(filtered_list.last().expect("Filtered list is empty").to_string())
}

pub async fn pb_list_impl<I>(
    auth: &AuthFlowChoice,
    override_base_url: Option<String>,
    version: Option<String>,
    branch: Option<String>,
    ui: &I,
) -> Result<Vec<ProductBundle>>
where
    I: structured_ui::Interface,
{
    let client = Client::initial()?;

    let mut products = Vec::new();
    let base_urls = if let Some(base_url) = override_base_url {
        vec![base_url]
    } else {
        resolve_branch_to_base_urls(version, branch, auth, ui, &client).await?
    };

    for base_url in &base_urls {
        let prods = pb_gather_from_url(base_url, auth, ui, &client).await.unwrap_or_else(|_| {
            println!("Failed to fetch from base_url: {}", &base_url);
            Vec::new()
        });
        products.extend(prods);
    }
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
    I: structured_ui::Interface,
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
    use {super::*, ffx_writer::Format, temp_test_env::TempTestEnv};

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
            Some(String::from("fake_version")),
            None,
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
                branch: None,
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
