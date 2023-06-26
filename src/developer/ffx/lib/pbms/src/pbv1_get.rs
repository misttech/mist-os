// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    get_images_dir, get_product_data, get_product_dir, handle_new_access_token, pbm_repo_list,
    select_auth, AuthFlowChoice,
};
use anyhow::{bail, Context, Result};
use ffx_config::Sdk;
use gcs::{client::Client, error::GcsError};
use sdk_metadata::{from_reader, Metadata};
use url::Url;

pub async fn bundled_exists_locally(sdk: &ffx_config::Sdk, product_url: &Url) -> Result<bool> {
    let path = get_images_dir(&product_url, sdk.get_path_prefix()).await;
    Ok(path.is_ok() && path.unwrap().exists())
}

/// Downloads the selected product bundle contents to the local storage directory.
/// Returns:
///   - Err(_) if the download fails unexpectedly.
///   - Ok(false) if the download is skipped because the product bundle is already
///     available in local storage.
///   - Ok(true) if the files are successfully downloaded.
pub async fn download_product_bundle<I>(
    sdk: &ffx_config::Sdk,
    ui: &mut I,
    oob_auth: bool,
    auth_flow: &AuthFlowChoice,
    product_url: &Url,
) -> Result<bool>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("download_product_bundle: oob_auth: {oob_auth} auth_flow: {auth_flow:?} product_url: {product_url:?}");
    let output_dir = get_product_dir(&product_url).await?;

    // Check if the bundle is already downloaded, and if so, return OK.
    if bundled_exists_locally(sdk, product_url).await? {
        return Ok(false);
    }

    let client = Client::initial()?;
    get_product_data(&product_url, &output_dir, select_auth(oob_auth, auth_flow), ui, &client).await
}

/// Downloads a legacy product bundle based on a
/// version prefix and product name.
pub async fn download<I: structured_ui::Interface>(
    sdk: &Sdk,
    auth: &AuthFlowChoice,
    version: String,
    product_name: &str,
    client: &Client,
    ui: &mut I,
) -> Result<()> {
    let mut product_url: Option<String> = None;

    // This list is the same as in fremote list-images.
    // gs://fuchsia/development/11.20230307.3.1/sdk/product_bundles.json#core.x64-dfv2

    // Use the repo list to get the GCS bucket(s) for the product bundles. Since this is a legacy product bundle,
    // we know the index of products is at development/$version/sdk/product_bundles.json, so that is hard coded to
    // simplify the implementation.
    let bundle_sources: Vec<Url> = pbm_repo_list(&sdk)
        .await
        .unwrap()
        .iter()
        .filter_map(|p| if p.as_str().contains("gs://") { Some(p.clone()) } else { None })
        .collect();
    tracing::debug!("legacy bundle sources {bundle_sources:?}");

    // Loop over the sources looking for the product name.
    // Use an iterator to allow retrying if the auth token needs to be refreshed.
    let mut source_iter = bundle_sources.iter();
    let mut src = source_iter.next();
    while src.is_some() {
        let bucket = src.expect("bundle repo source").domain().unwrap();
        match find_latest_build(&client, &auth, ui, bucket, &version, &product_name).await {
            Ok(Some(pb_index)) => {
                tracing::debug!("Product url found: {pb_index}");
                product_url = Some(pb_index);
                break;
            }
            Ok(None) => {
                tracing::debug!("Product bundle not found in {bucket}");
                src = source_iter.next();
            }
            // NewAccessToken is handled in find_latest_build, so
            // all we do here it loop.
            Err(e) => match e.downcast_ref::<GcsError>() {
                Some(GcsError::NeedNewAccessToken) => (),
                _ => return Err(e),
            },
        }
    }

    // If the product is found, download it if needed.
    if let Some(product) = product_url {
        let url = Url::parse(&product)?;
        if bundled_exists_locally(sdk, &url).await? {
            println!("Product bundle {product} is already downloaded");
            return Ok(());
        }
        println!("Downloading {product}");
        let oob_auth = auth == &AuthFlowChoice::Oob;
        download_product_bundle(sdk, ui, oob_auth, auth, &url).await?;
        Ok(())
    } else {
        bail!("Could not find a legacy product bundle with version {version} for product {product_name}")
    }
}

/// Finds the latest version that matches the version passed in by sorting and then
/// takes the last entry. Once it has the file, it is loaded to check for the product_name.
async fn find_latest_build<I: structured_ui::Interface>(
    client: &Client,
    auth: &AuthFlowChoice,
    ui: &I,
    bucket: &str,
    version: &str,
    product_name: &str,
) -> Result<Option<String>> {
    let res = client.list(bucket, &format!("development/{version}")).await;
    match res {
        Ok(version_list) => {
            let mut list: Vec<_> = version_list
                .iter()
                .filter_map(|p| {
                    if p.contains("sdk/product_bundles.json") {
                        Some(p.clone())
                    } else {
                        None
                    }
                })
                .collect();
            list.sort();
            if let Some(latest_path) = list.last() {
                let parts = latest_path.split('/');
                let latest_version = parts.collect::<Vec<&str>>()[1];
                let index_url = format!("development/{latest_version}/sdk/product_bundles.json");
                let bundle_names = read_names_from_index(bucket, &index_url, &client).await?;
                if bundle_names.contains(&product_name.to_string()) {
                    Ok(Some(format!("gs://{bucket}/{index_url}#{product_name}")))
                } else {
                    tracing::debug!("{product_name} not found in {bucket}/{index_url}. Names are {bundle_names:?}");
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }
        Err(e) => {
            // GcsError does not implement Eq or PartialEq, so use a match vs. if.
            match e.downcast_ref::<GcsError>() {
                Some(GcsError::NeedNewAccessToken) => {
                    tracing::debug!("got NeedNewAccessToken, setting access token in client.");
                    let access_token = handle_new_access_token(auth, ui)
                        .await
                        .context("Getting new access token.")?;
                    client.set_access_token(access_token).await;
                }
                _ => (),
            };
            return Err(e);
        }
    }
}

/// Reads the product names from the product_bundles.json file.
/// It is assumed that any authentication steps have already been taken.
async fn read_names_from_index(
    bucket: &str,
    index_url: &str,
    client: &Client,
) -> Result<Vec<String>> {
    let mut response = client.stream(bucket, index_url).await?;
    if response.status().is_success() {
        let bytes = hyper::body::to_bytes(response.body_mut()).await.unwrap();
        let data = String::from_utf8(bytes.into_iter().collect()).expect("");
        let metadata: Metadata = from_reader(&mut data.as_bytes())?;
        Ok(metadata.get_product_bundles().iter().map(|s| s.to_string()).collect())
    } else {
        bail!("Error reading bundle index from {bucket}: {}", response.status());
    }
}
