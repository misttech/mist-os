// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{get_images_dir, get_product_data, get_product_dir, select_auth, AuthFlowChoice};
use anyhow::Result;
use gcs::client::Client;
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
