// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Access utilities for product metadata.
//!
//! This is a collection of helper functions wrapping the FMS and GCS libs.
//!
//! The metadata can be loaded from a variety of sources. The initial places are
//! GCS and the local build.
//!
//! Call `product_bundle_urls()` to get a set of URLs for each product bundle.
//!
//! Call `fms_entries_from()` to get FMS entries from a particular repo. The
//! entries include product bundle metadata, physical device specifications, and
//! virtual device specifications. Each FMS entry has a unique name to identify
//! that entry.
//!
//! These FMS entry names are suitable to present to the user. E.g. the name of
//! a product bundle is also the name of the product bundle metadata entry.

use crate::gcs::string_from_gcs;
use crate::pbms::{local_path_helper, path_from_file_url, GS_SCHEME};
use ::gcs::client::{Client, FileProgress, ProgressResult};
use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_bail;
use hyper::{Body, Method, Request};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use crate::gcs::{handle_new_access_token, list_from_gcs};
pub use crate::pbms::{get_product_dir, get_storage_dir};
pub use crate::transfer_manifest::transfer_download;
pub use sdk_metadata::{LoadedProductBundle, ProductBundle};

mod gcs;
mod pbms;
pub mod transfer_manifest;
/// Select an Oauth2 authorization flow.
#[derive(PartialEq, Debug, Clone)]
pub enum AuthFlowChoice {
    /// Fail rather than using authentication.
    NoAuth,
    Default,
    Device,
    Exec(PathBuf),
    Pkce,
}

const PRODUCT_BUNDLE_PATH_KEY: &str = "product.path";

/// Convert CLI arg or config strings to AuthFlowChoice.
impl FromStr for AuthFlowChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.as_ref() {
            "no-auth" => Ok(AuthFlowChoice::NoAuth),
            "default" => Ok(AuthFlowChoice::Default),
            "device-experimental" => Ok(AuthFlowChoice::Device),
            "pkce" => Ok(AuthFlowChoice::Pkce),
            exec => {
                let path = Path::new(exec);
                if path.is_file() {
                    Ok(AuthFlowChoice::Exec(path.to_path_buf()))
                } else {
                    Err("Unknown auth flow choice. Use one of oob, \
                        device-experimental, pkce, default, a path to an \
                        executable which prints an access token to stdout, or \
                        no-auth to enforce that no auth flow will be used."
                        .to_string())
                }
            }
        }
    }
}

#[derive(PartialEq, Debug, Clone)]
pub enum ListingMode {
    AllBundles,
    GetableBundles,
    ReadyBundlesOnly,
    RemovableBundles,
}

pub fn is_local_product_bundle<P: AsRef<Path>>(product_bundle: P) -> bool {
    product_bundle.as_ref().exists()
}

/// Load a product bundle by name, uri, or local path.
///
/// If a build config value is set for product.path and the product bundle
/// is None, this method will use the value that is set in the build config as
/// the path.
/// If the path is absolute it will be used as is.
/// otherwise the relative path is checked to exist, and if it does not,
/// it is joined to the build_dir. This way, command line references to relative
/// paths work as developer's expect, and still maintain the legacy behavior.
pub async fn load_product_bundle(product_bundle: &Option<String>) -> Result<LoadedProductBundle> {
    // Can't use unwrap_or_else here since ffx config get is async.
    let bundle_path: Utf8PathBuf = match product_bundle {
        Some(p) => p.into(),
        None => ffx_config::get::<String, &str>(PRODUCT_BUNDLE_PATH_KEY).unwrap_or_default().into(),
    };

    if bundle_path.as_std_path() == Path::new("") {
        anyhow::bail!("No product bundle path configured, nor specified.");
    }

    tracing::debug!("Loading a product bundle: {:?}", bundle_path);

    if is_local_product_bundle(&bundle_path) {
        return LoadedProductBundle::try_load_from(&bundle_path);
    } else if bundle_path.is_relative() {
        let env = ffx_config::global_env_context().expect("cannot get global_env_context");

        if let Some(base_path) = env.build_dir().map(Utf8Path::from_path).flatten() {
            let base_dir_based_path: Utf8PathBuf = base_path.join(&bundle_path);

            if is_local_product_bundle(&base_dir_based_path) {
                return LoadedProductBundle::try_load_from(&base_dir_based_path);
            } else {
                anyhow::bail!(
                    "Could not find product bundle in {bundle_path:?} nor {base_dir_based_path:?}"
                );
            }
        }
    }
    anyhow::bail!("Could not find product bundle in {bundle_path:?}");
}

/// Determine if a product bundle url refers to a locally-built bundle.
///
/// Note that this is a heuristic for PBv1 only. It assumes that only a locally-built bundle
/// will be have a source URL with a "file" scheme. The implementation will likely change with
/// PBv2.
pub fn is_locally_built(product_url: &url::Url) -> bool {
    product_url.scheme() == "file"
}
pub async fn select_product_bundle(
    _sdk: &ffx_config::Sdk,
    _looking_for: &Option<String>,
    _mode: ListingMode,
    _should_print: bool,
) -> Result<url::Url> {
    panic!("product bundle v1 support not compiled in this build. Rebuild with  build_pb_v1=true")
}

/// Determine whether the data for `product_url` is downloaded and ready to be
/// used.
pub async fn is_pb_ready(product_url: &url::Url, sdk_root: &Path) -> Result<bool> {
    assert!(product_url.as_str().contains("#"));
    Ok(get_images_dir(product_url, sdk_root).await.context("getting images dir")?.is_dir())
}

/// Determine the path to the product images data.
pub async fn get_images_dir(product_url: &url::Url, sdk_root: &Path) -> Result<PathBuf> {
    assert!(!product_url.as_str().is_empty());
    let name = product_url.fragment().expect("a URI fragment is required");
    assert!(!name.is_empty());
    assert!(!name.contains("/"));
    local_path_helper(product_url, &format!("{}/images", name), /*dir=*/ true, sdk_root).await
}

/// Determine the path to the product packages data.
pub async fn get_packages_dir(product_url: &url::Url, sdk_root: &Path) -> Result<PathBuf> {
    assert!(!product_url.as_str().is_empty());
    let name = product_url.fragment().expect("a URI fragment is required");
    assert!(!name.is_empty());
    assert!(!name.contains("/"));
    local_path_helper(product_url, &format!("{}/packages", name), /*dir=*/ true, sdk_root).await
}

/// Determine the path to the local product metadata directory.
pub async fn get_metadata_dir(product_url: &url::Url, sdk_root: &Path) -> Result<PathBuf> {
    assert!(!product_url.as_str().is_empty());
    assert!(!product_url.fragment().is_none());
    Ok(get_metadata_glob(product_url, sdk_root)
        .await
        .context("getting metadata")?
        .parent()
        .expect("Metadata files should have a parent")
        .to_path_buf())
}

/// Determine the glob path to the product metadata.
///
/// A glob path may have wildcards, such as "file://foo/*.json".
pub async fn get_metadata_glob(product_url: &url::Url, sdk_root: &Path) -> Result<PathBuf> {
    assert!(!product_url.as_str().is_empty());
    assert!(!product_url.fragment().is_none());
    local_path_helper(product_url, "product_bundles.json", /*dir=*/ false, sdk_root).await
}

/// Remove prior output directory, if necessary.
pub async fn make_way_for_output(local_dir: &Path, force: bool) -> Result<()> {
    tracing::debug!("make_way_for_output {:?}, force {}", local_dir, force);
    if local_dir.exists() {
        tracing::debug!("local_dir.exists {:?}", local_dir);
        if std::fs::read_dir(&local_dir).expect("reading dir").next().is_none() {
            tracing::debug!("local_dir is empty (which is good) {:?}", local_dir);
            return Ok(());
        } else if force {
            if local_dir == Path::new("") || local_dir == Path::new("/") {
                ffx_bail!(
                    "The output directory is {:?} which looks like a mistake. \
                    Please try a different output directory path.",
                    local_dir
                );
            }
            if !local_dir.join("product_bundle.json").exists() {
                ffx_bail!(
                    "The directory does not resemble an old product \
                    bundle. For caution's sake, please remove the output \
                    directory {:?} by hand and try again.",
                    local_dir
                );
            }
            async_fs::remove_dir_all(&local_dir)
                .await
                .with_context(|| format!("removing output dir {:?}", local_dir))?;
            tracing::debug!("Removed all of {:?}", local_dir);
        } else {
            ffx_bail!(
                "The output directory already exists. Please provide \
                another directory to write to, or use --force to overwrite the \
                contents of {:?}.",
                local_dir
            );
        }
    }
    tracing::debug!("local_dir dir clear.");
    Ok(())
}

/// Download data from any of the supported schemes listed in RFC-100, Product
/// Bundle, "bundle_uri" to a string.
///
/// Currently: "pattern": "^(?:http|https|gs|file):\/\/"
///
/// Note: If the contents are large or more than a single file is expected,
/// consider using fetch_from_url to write to a file instead.
pub async fn string_from_url<F, I>(
    product_url: &url::Url,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<String>
where
    F: Fn(FileProgress<'_>) -> ProgressResult,
    I: structured_ui::Interface,
{
    tracing::debug!("string_from_url {}", product_url);
    Ok(match product_url.scheme() {
        "http" | "https" => {
            let https_client = fuchsia_hyper::new_https_client();
            let req = Request::builder()
                .method(Method::GET)
                .uri(product_url.as_str())
                .body(Body::empty())?;
            let res = https_client.request(req).await?;
            if !res.status().is_success() {
                bail!("http(s) request failed, status {}, for {}", res.status(), product_url);
            }
            let bytes = hyper::body::to_bytes(res.into_body()).await?;
            String::from_utf8_lossy(&bytes).to_string()
        }
        GS_SCHEME => string_from_gcs(product_url.as_str(), auth_flow, progress, ui, client)
            .await
            .context("Downloading from GCS as string.")?,
        "file" => {
            if let Some(file_path) = &path_from_file_url(product_url) {
                std::fs::read_to_string(file_path)
                    .with_context(|| format!("string_from_url reading {:?}", file_path))?
            } else {
                bail!(
                    "Invalid URL (e.g.: 'file://foo', with two initial slashes is invalid): {}",
                    product_url
                )
            }
        }
        _ => bail!("Unexpected URI scheme in ({:?})", product_url),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_config::environment::test_init_in_tree;
    use ffx_config::ConfigLevel;
    use std::fs::File;
    use tempfile::TempDir;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_intree_errors() {
        let test_dir = TempDir::new().expect("output directory");
        let build_dir =
            Utf8Path::from_path(test_dir.path()).expect("cannot convert builddir to Utf8Path");
        let _env = test_init_in_tree(&test_dir.path()).await.unwrap();

        let empty_pb: Option<String> = None;
        // If no product bundle path provided and no config return None
        let pb = load_product_bundle(&empty_pb).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            "No product bundle path configured, nor specified."
        );

        // If pb provided but invalid path return None
        let pb_path = build_dir.join("__invalid__").to_string();
        let pb = load_product_bundle(&Some(pb_path.clone())).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            format!("Could not find product bundle in \"{pb_path}\"")
        );

        // If pb provided and absolute and valid return Some(abspath)
        let pb = load_product_bundle(&Some(build_dir.to_string())).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            format!("No such file or directory (os error 2): \"{build_dir}/product_bundle.json\"")
        );

        // If pb provided, relative and but to a file not a directory.
        let relpath = "foo".to_string();
        std::fs::File::create(build_dir.join(relpath.clone())).expect("create relative dir");
        let pb = load_product_bundle(&Some(relpath.clone())).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            format!("{}/{relpath} is not a directory", build_dir.to_string())
        );

        // If pb provided and relative and invalid return None
        let pb = load_product_bundle(&Some("invalid".into())).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            format!(
                "Could not find product bundle in \"invalid\" nor \"{}/invalid\"",
                build_dir.to_string()
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_no_build_dir() {
        let _env = ffx_config::test_init().await.unwrap();

        // Can handle an empty build path
        let pb = load_product_bundle(&Some("some_place".to_string())).await;
        assert_eq!(
            pb.err().unwrap().to_string(),
            format!("Could not find product bundle in \"some_place\"")
        );
    }

    #[test]
    fn test_is_local_product_bundle() {
        let temp_dir = TempDir::new().expect("temp dir");
        let temp_path = temp_dir.path();

        assert!(is_local_product_bundle(temp_path.as_os_str().to_str().unwrap()));
        assert!(!is_local_product_bundle("gs://fuchsia/test_fake.tgz"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_make_way_for_output() {
        let test_dir = tempfile::TempDir::new().expect("temp dir");

        make_way_for_output(&test_dir.path(), /*force=*/ false).await.expect("empty dir is okay");

        std::fs::create_dir(&test_dir.path().join("foo")).expect("make_dir foo");
        std::fs::File::create(test_dir.path().join("info")).expect("create info");
        std::fs::File::create(test_dir.path().join("product_bundle.json"))
            .expect("create product_bundle.json");
        make_way_for_output(&test_dir.path(), /*force=*/ true).await.expect("rm dir is okay");

        let test_dir = tempfile::TempDir::new().expect("temp dir");
        std::fs::create_dir(&test_dir.path().join("foo")).expect("make_dir foo");
        assert!(make_way_for_output(&test_dir.path(), /*force=*/ false).await.is_err());
    }

    macro_rules! make_pb_v2_in {
        ($dir:expr,$name:expr)=>{
            {
                let pb_dir = Utf8Path::from_path($dir.path()).unwrap();
                let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
                serde_json::to_writer(
                    &pb_file,
                    &serde_json::json!({
                        "version": "2",
                        "product_name": $name,
                        "product_version": "version",
                        "sdk_version": "sdk-version",
                        "partitions": {
                            "hardware_revision": "board",
                            "bootstrap_partitions": [],
                            "bootloader_partitions": [],
                            "partitions": [],
                            "unlock_credentials": [],
                        },
                    }),
                )
                .unwrap();
                pb_dir
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_v2_valid() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "fake.x64");

        let env = ffx_config::test_init().await.expect("create test config");
        // Load with passing a path directly
        let pb = load_product_bundle(&Some(pb_dir.to_string()))
            .await
            .expect("could not load product bundle");
        assert_eq!(pb.loaded_from_path(), pb_dir);

        // Load with the config set with absolute path
        env.context
            .query(PRODUCT_BUNDLE_PATH_KEY)
            .level(Some(ConfigLevel::User))
            .set(serde_json::Value::String(pb_dir.to_string()))
            .await
            .expect("set product.path path");
        let pb = load_product_bundle(&None).await.expect("could not load product bundle");
        assert_eq!(pb.loaded_from_path(), pb_dir);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_v2_invalid() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = Utf8Path::from_path(tmp.path()).unwrap();
        let _env = ffx_config::test_init().await.expect("create test config");

        // Load with passing a path directly
        let pb = load_product_bundle(&Some(pb_dir.to_string())).await;
        assert!(pb.is_err());
    }
}
