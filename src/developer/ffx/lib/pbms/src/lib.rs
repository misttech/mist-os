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

use crate::{
    gcs::string_from_gcs,
    pbms::{
        fetch_product_metadata, get_product_data_from_gcs, local_path_helper, path_from_file_url,
        pb_dir_name, pb_names_from_path, pbm_repo_list, CONFIG_STORAGE_PATH, GS_SCHEME,
    },
    repo_info::RepoInfo,
};
use ::gcs::client::{Client, FileProgress, ProgressResponse, ProgressResult};
use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_bail;
use fms::Entries;
use futures::TryStreamExt as _;
use hyper::{Body, Method, Request};
#[cfg(feature = "build_pb_v1")]
use itertools::Itertools as _;
#[cfg(feature = "build_pb_v1")]
use sdk;
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};
use structured_ui::{Presentation, TableRows};

pub use crate::{
    gcs::handle_new_access_token,
    pbms::{fetch_data_for_product_bundle_v1, get_product_dir, get_storage_dir},
    transfer_manifest::transfer_download,
};
pub use sdk_metadata::{LoadedProductBundle, ProductBundle};

mod gcs;
mod pbms;
pub mod pbv1_get;
mod repo;
mod repo_info;
pub mod transfer_manifest;
/// Select an Oauth2 authorization flow.
#[derive(PartialEq, Debug, Clone)]
pub enum AuthFlowChoice {
    /// Fail rather than using authentication.
    NoAuth,
    Default,
    Device,
    Exec(PathBuf),
    Oob,
    Pkce,
}

const PRODUCT_BUNDLE_PATH_KEY: &str = "product.path";

/// Convert common cli switches to AuthFlowChoice.
pub fn select_auth(oob_auth: bool, auth_flow: &AuthFlowChoice) -> &AuthFlowChoice {
    if oob_auth {
        eprintln!("\n\nPlease use `--auth oob` rather than `--oob-auth`\n\n");
        &AuthFlowChoice::Oob
    } else {
        auth_flow
    }
}

/// Convert CLI arg or config strings to AuthFlowChoice.
impl FromStr for AuthFlowChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.as_ref() {
            "no-auth" => Ok(AuthFlowChoice::NoAuth),
            "default" => Ok(AuthFlowChoice::Default),
            "device-experimental" => Ok(AuthFlowChoice::Device),
            "oob" => Ok(AuthFlowChoice::Oob),
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
/// This is capable of loading both v1 and v2 ProductBundles.
///
/// If a build config value is set for product.path and the product bundle
/// is None, this method will use the value that is set in the build config as
/// the path. If the path is absolute it will be used as is, otherwise it will
/// be relative to the build_dir.
pub async fn load_product_bundle(
    _sdk: &ffx_config::Sdk,
    product_bundle: &Option<String>,
    _mode: ListingMode,
) -> Result<LoadedProductBundle> {
    tracing::debug!("Loading a product bundle: {:?}", product_bundle);
    let env = ffx_config::global_env_context().expect("cannot get global_env_context");
    let build_dir = env.build_dir().map(Utf8Path::from_path).flatten().unwrap_or(Utf8Path::new(""));

    //  If `product_bundle` is a local path, load it directly.
    if let Some(path) = local_product_bundle_path(product_bundle, build_dir).await {
        return LoadedProductBundle::try_load_from(path);
    }

    #[cfg(feature = "build_pb_v1")]
    {
        // Otherwise, use the `fms` crate to fetch and parse the product bundle by name or uri.
        let should_print = true;
        let product_url = select_product_bundle(_sdk, product_bundle, _mode, should_print)
            .await
            .context("Selecting product bundle")?;

        let name = product_url.fragment().expect("Product name is required.");

        let fms_entries = fms_entries_from(&product_url, &_sdk.get_path_prefix())
            .await
            .context("get fms entries")?;

        let product = fms::find_product_bundle(&fms_entries, &Some(name.to_string()))
            .context("problem with product_bundle")?
            .to_owned();
        // The metadata glob has the form /path/to/*.json and we want to remove
        // the last path part.
        let mut path = get_metadata_glob(&product_url, &_sdk.get_path_prefix())
            .await
            .expect("unable to get metadata glob");
        path.pop();

        Ok(LoadedProductBundle::new(
            ProductBundle::V1(product),
            Utf8PathBuf::from_path_buf(path).unwrap(),
        ))
    }
    #[cfg(not(feature = "build_pb_v1"))]
    bail!("A path to a product bundle v2 is required because `build_pb_v1` was false");
}

/// Checks to see if the product bundle is valid. If not product bundle is set then
/// we will check the config domains to see if a "product.path" key is present.
/// If the product bundle does not exist then None is returned.
#[allow(dead_code)]
async fn local_product_bundle_path(
    product_bundle: &Option<impl AsRef<Utf8Path>>,
    build_dir: impl AsRef<Utf8Path>,
) -> Option<Utf8PathBuf> {
    match product_bundle {
        Some(p) => Some(Utf8PathBuf::from(p.as_ref())),
        None => ffx_config::get::<String, &str>(PRODUCT_BUNDLE_PATH_KEY)
            .await
            .ok()
            .map(Utf8PathBuf::from),
    }
    .map(|s| resolve_product_bundle_path(s, build_dir))
    .filter(|s| is_local_product_bundle(s))
}

/// Resolves the product bundle relative to the build_dir. If product_bundle is
/// absolute return it as is otherwise rebase off of the build_dir.
fn resolve_product_bundle_path(
    product_bundle: impl AsRef<Utf8Path>,
    build_dir: impl AsRef<Utf8Path>,
) -> Utf8PathBuf {
    let path = product_bundle.as_ref();
    if path.is_absolute() {
        return path.into();
    } else {
        return build_dir.as_ref().join(path);
    }
}

/// For each non-local URL in ffx CONFIG_METADATA, fetch updated info.
pub async fn update_metadata_all<I>(
    sdk: &ffx_config::Sdk,
    output_dir: &Path,
    auth_flow: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("update_metadata_all");
    let repos = pbm_repo_list(sdk).await.context("getting repo list")?;
    async_fs::create_dir_all(&output_dir).await.context("create directory")?;
    for repo_url in repos {
        if repo_url.scheme() != GS_SCHEME {
            // There's no need to fetch local files or unrecognized schemes.
            continue;
        }
        tracing::debug!("update_metadata_all repo_url {:?}", repo_url);
        fetch_product_metadata(
            &repo_url,
            &output_dir.join(pb_dir_name(&repo_url)),
            auth_flow,
            &|_d, _f| Ok(ProgressResponse::Continue),
            ui,
            client,
        )
        .await
        .context("fetching product metadata")?;
    }
    Ok(())
}

/// Update metadata from given url.
pub async fn update_metadata_from<I>(
    product_url: &url::Url,
    output_dir: &Path,
    auth_flow: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("update_metadata_from");
    fetch_product_metadata(
        &product_url,
        output_dir,
        &auth_flow,
        &|_d, _f| Ok(ProgressResponse::Continue),
        ui,
        client,
    )
    .await
    .context("fetching product metadata")
}

/// Gather a list of PBM reference URLs which include the product bundle entry
/// name.
///
/// Tip: Call `update_metadata()` to update the info (or not, if the intent is
///      to peek at what's there without updating).
pub async fn product_bundle_urls(sdk: &ffx_config::Sdk) -> Result<Vec<url::Url>> {
    tracing::debug!("product_bundle_urls");
    let mut result = Vec::new();

    // Collect product bundle URLs from the file paths in ffx config.
    for repo in pbm_repo_list(&sdk).await.context("getting repo list")? {
        if let Some(path) = path_from_file_url(&repo) {
            let names = pb_names_from_path(&path).context("loading product bundle names")?;
            for name in names {
                let mut product_url = repo.to_owned();
                product_url.set_fragment(Some(&name));
                result.push(product_url);
            }
        }
    }

    let storage_path: PathBuf =
        ffx_config::get(CONFIG_STORAGE_PATH).await.context("getting CONFIG_STORAGE_PATH")?;
    if !storage_path.is_dir() {
        // Early out before calling read_dir.
        return Ok(result);
    }

    // Collect product bundle URLs from the downloaded information. These
    // entries may not be currently referenced in the ffx config. This is where
    // product bundles from old versions will be pulled in, for example.
    let mut dir_entries = async_fs::read_dir(storage_path).await.context("reading vendors dir")?;
    while let Some(dir_entry) = dir_entries.try_next().await.context("reading directory")? {
        if dir_entry.path().is_dir() {
            if let Ok(repo_info) = RepoInfo::load(&dir_entry.path().join("info")) {
                let names = pb_names_from_path(&dir_entry.path().join("product_bundles.json"))?;
                for name in names {
                    let repo = format!("{}#{}", repo_info.metadata_url, name);
                    result.push(
                        url::Url::parse(&repo)
                            .with_context(|| format!("parsing metadata URL {:?}", repo))?,
                    );
                }
            }
        }
    }
    Ok(result)
}

/// Gather all the fms entries from a given product_url.
///
/// If `product_url` is None or not a URL, then an attempt will be made to find
/// default entries.
pub async fn fms_entries_from(product_url: &url::Url, sdk_root: &Path) -> Result<Entries> {
    tracing::debug!("fms_entries_from");
    let path = get_metadata_glob(product_url, sdk_root).await.context("getting metadata")?;
    let mut entries = Entries::new();
    entries.add_from_path(&path).context("adding entries")?;
    Ok(entries)
}

/// The default behavior for when there are more than one matching
/// product-bundle.
///
/// This function should only be called with an iterator of `url::Url`s that has
/// 2 or more entries, and has already been sorted & reversed.
#[cfg(feature = "build_pb_v1")]
fn default_pbm_of_many<I>(
    mut urls: I,
    sdk_version: &sdk::SdkVersion,
    looking_for: Option<String>,
    should_print: bool,
) -> Result<url::Url>
where
    I: Iterator<Item = url::Url> + Clone,
{
    let extra_message = if let Some(looking_for) = looking_for {
        format!(" for `{}`", looking_for)
    } else {
        String::new()
    };
    let formatted = urls.clone().map(|url| format!("{}", url)).collect::<Vec<String>>().join("\n");
    if should_print {
        println!(
            "Multiple product bundles found{extra_message}. To choose a specific product, pass \
            in a full URL from the following:\n\n{formatted}",
            extra_message = extra_message,
            formatted = formatted
        );
    }
    tracing::info!("Product bundles: {}", formatted);
    // If the user is working in-tree, we want to default to any available locally-built bundles.
    let mut selected: Option<(url::Url, &str)> = if sdk_version == &sdk::SdkVersion::InTree {
        tracing::info!("In-tree selection");
        let mut local_builds = urls.clone().filter(|url| is_locally_built(&url));
        match local_builds.next() {
            Some(first) => Some((first, "the locally-built product bundle")),
            None => None,
        }
    } else {
        None
    };
    // If there is no locally-built bundle, then we want to match the current SDK version.
    if selected.is_none() {
        selected = match sdk_version {
            sdk::SdkVersion::Version(version) => {
                tracing::info!("Sdk version: `{}`", version);
                let mut matching_sdk_version = urls.filter(|url| url.to_string().contains(version));
                let first = matching_sdk_version.next();
                match first {
                    Some(first) => {
                        Some((first, "the first valid product bundle for this SDK version"))
                    }
                    None => {
                        bail!("There were no product-bundles for your sdk version: `{}`", version)
                    }
                }
            }
            sdk::SdkVersion::InTree | sdk::SdkVersion::Unknown => match urls.next() {
                Some(first) => Some((first, "the first valid product bundle in sorted order")),
                None => {
                    bail!(
                        "This function should only be called with an iterator with at least 2 entries."
                    )
                }
            },
        }
    };
    // If we get here, selected is Some(_,_); if we didn't match, we threw an error.
    let (url, description) = selected.unwrap();
    if should_print {
        println!("\nDefaulting to {}: `{}`", description, url);
    }
    Ok(url)
}

/// Determine if a product bundle url refers to a locally-built bundle.
///
/// Note that this is a heuristic for PBv1 only. It assumes that only a locally-built bundle
/// will be have a source URL with a "file" scheme. The implementation will likely change with
/// PBv2.
pub fn is_locally_built(product_url: &url::Url) -> bool {
    product_url.scheme() == "file"
}

/// Find a product bundle url and name for `product_url`.
///
/// If product_url is
/// - None and there is only one product bundle available, use it.
/// - Some(product name) and a url with that fragment is found, use it.
/// - Some(full product url with fragment) use it.
/// If a match is not found or multiple matches are found, fail with an error
/// message.
///
/// Tip: Call `update_metadata()` to get up to date choices (or not, if the
///      intent is to select from what's already there).
#[cfg(feature = "build_pb_v1")]
pub async fn select_product_bundle(
    sdk: &ffx_config::Sdk,
    looking_for: &Option<String>,
    mode: ListingMode,
    should_print: bool,
) -> Result<url::Url> {
    tracing::debug!("select_product_bundle");

    let sdk_version = sdk.get_version();
    let sdk_root = sdk.get_path_prefix();

    // URLs for the product bundle are locked to the version of the sdk.
    // The looking_for parameter is an exact match on one of these URLs,
    // or a fragment of the URL.
    let mut urls = product_bundle_urls(sdk).await.context("getting product bundle URLs")?;

    tracing::debug!("matching {looking_for:?} in {urls:?}");
    // Sort the URLs lexicographically, then reverse them so the most recent
    // version strings will be first.
    urls.sort();
    urls.reverse();
    let mut iter = urls.into_iter();
    if mode == ListingMode::ReadyBundlesOnly || mode == ListingMode::RemovableBundles {
        // Unfortunately this can't be a filter() because is_pb_ready is async.
        let mut ready = Vec::new();
        for url in iter {
            if is_pb_ready(&url, sdk_root).await? {
                ready.push(url);
            }
        }
        iter = ready.into_iter();
    }
    // The locally built bundle can't be removed or downloaded, so skip it for those cases.
    let iter = iter.filter(|url| {
        mode == ListingMode::AllBundles
            || mode == ListingMode::ReadyBundlesOnly
            || !is_locally_built(&url)
    });
    if let Some(looking_for) = &looking_for {
        let matches = iter.filter(|url| {
            return url.as_str() == looking_for
                || url.fragment().expect("product_urls must have fragment") == looking_for;
        });
        match matches.at_most_one() {
            Ok(Some(m)) => Ok(m),
            Ok(None) => bail!(
                "A product bundle with that name was not found, please check the spelling and try again."
            ),
            Err(matches) => default_pbm_of_many(matches, sdk_version, Some(looking_for.to_string()), should_print),
        }
    } else {
        match iter.at_most_one() {
            Ok(Some(url)) => Ok(url),
            Ok(None) => bail!("There are no product bundles available."),
            Err(urls) => default_pbm_of_many(urls, sdk_version, None, should_print),
        }
    }
}

#[cfg(not(feature = "build_pb_v1"))]
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

/// Download data related to the product.
///
/// The emulator may then be run with the data downloaded.
///
/// If `product_bundle_url` is None and only one viable PBM is available, that entry
/// is used.
///
/// `writer` is used to output user messages.
pub async fn get_product_data<I>(
    product_url: &url::Url,
    output_dir: &std::path::Path,
    auth_flow: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<bool>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("get_product_data {:?} to {:?}", product_url, output_dir);
    let mut should_get_data = true;
    let mut note = TableRows::builder();
    if is_locally_built(&product_url) {
        note.title("There's no data download necessary for local products.");
        should_get_data = false;
    } else if product_url.scheme() != GS_SCHEME {
        note.title("Only GCS downloads are supported at this time.");
        should_get_data = false;
    }
    if should_get_data {
        get_product_data_from_gcs(product_url, output_dir, auth_flow, ui, client)
            .await
            .context("reading pbms entries")
            .map(|_| true)
    } else {
        ui.present(&Presentation::Table(note)).expect("Problem presenting the note.");
        Ok(false)
    }
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
    assert!(
        !local_dir.exists(),
        "The local_dir exists in make_way_for_output, please report this as a bug."
    );
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
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("string_from_url {:?}", product_url);
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
    use crate::pbms::CONFIG_METADATA;
    use ffx_config::ConfigLevel;
    use serde_json;
    use std::{fs::File, io::Write};
    use tempfile::TempDir;

    const CORE_JSON: &str = include_str!("../test_data/test_core.json");
    const IMAGES_JSON: &str = include_str!("../test_data/test_images.json");
    const PRODUCT_BUNDLE_JSON: &str = include_str!("../test_data/test_product_bundle.json");

    fn create_test_intree_sdk() -> TempDir {
        let temp_dir = TempDir::new().expect("temp dir");
        let temp_path = temp_dir.path();

        let manifest_path = temp_dir.path().join("sdk/manifest");
        std::fs::create_dir_all(&manifest_path).expect("create dir");
        let mut core_file = File::create(manifest_path.join("core")).expect("create core");
        core_file.write_all(CORE_JSON.as_bytes()).expect("write core file");

        let mut images_file =
            File::create(temp_path.join("images.json")).expect("create images file");
        images_file.write_all(IMAGES_JSON.as_bytes()).expect("write images file");

        let mut pbm_file =
            File::create(temp_path.join("product_bundle.json")).expect("create images file");
        pbm_file.write_all(PRODUCT_BUNDLE_JSON.as_bytes()).expect("write pbm file");

        temp_dir
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_local_product_bundle_path() {
        let _env = ffx_config::test_init().await.unwrap();
        let test_dir = TempDir::new().expect("output directory");
        let build_dir =
            Utf8Path::from_path(test_dir.path()).expect("cannot convert builddir to Utf8Path");

        let empty_pb: Option<String> = None;
        // If no product bundle path provided and no config return None
        let pb = local_product_bundle_path(&empty_pb, build_dir).await;
        assert_eq!(pb, None);

        // If pb provided but invalid path return None
        let pb = local_product_bundle_path(&Some(build_dir.join("__invalid__")), build_dir).await;
        assert_eq!(pb, None);

        // If pb provided and absolute and valid return Some(abspath)
        let pb = local_product_bundle_path(&Some(build_dir), build_dir).await;
        assert_eq!(pb, Some(build_dir.into()));

        // If pb provided and relative and valid return Some(build_dir + relpath)
        let relpath = "foo";
        std::fs::File::create(build_dir.join(relpath)).expect("create relative dir");

        let pb = local_product_bundle_path(&Some(relpath), build_dir).await;
        assert_eq!(pb, Some(build_dir.join(relpath)));

        // If pb provided and relative and invalid return None
        let pb = local_product_bundle_path(&Some("invalid"), build_dir).await;
        assert_eq!(pb, None);

        // Can handle an empty build path
        let pb = local_product_bundle_path(&Some(build_dir), "").await;
        assert_eq!(pb, Some(build_dir.into()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_resolve_product_bundle_path() {
        let build_dir = "/out";

        // if absolute path then return it as is.
        let abspb = "/some/abs/path";
        assert_eq!(resolve_product_bundle_path(abspb, build_dir), Utf8Path::new(abspb));

        // if relative return the path relative to the build dir
        assert_eq!(resolve_product_bundle_path("foo", build_dir), Utf8Path::new("/out/foo"));
    }

    #[test]
    fn test_is_local_product_bundle() {
        let temp_dir = TempDir::new().expect("temp dir");
        let temp_path = temp_dir.path();

        assert!(is_local_product_bundle(temp_path.as_os_str().to_str().unwrap()));
        assert!(!is_local_product_bundle("gs://fuchsia/test_fake.tgz"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_pbms() {
        let env = ffx_config::test_init().await.expect("create test config");

        let sdk_root_dir = create_test_intree_sdk();
        let sdk_root = sdk_root_dir.path().to_str().expect("path to str");
        ffx_config::query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.into())
            .await
            .expect("set sdk root path");
        ffx_config::query("sdk.type")
            .level(Some(ConfigLevel::User))
            .set("in-tree".into())
            .await
            .expect("set sdk type");
        ffx_config::query(CONFIG_METADATA)
            .level(Some(ConfigLevel::User))
            .set(serde_json::json!(["{sdk.root}/*.json"]))
            .await
            .expect("set pbms metadata");

        let output_dir = TempDir::new().expect("output directory");
        let mut input = Box::new(std::io::stdin());
        let mut output = Vec::new();
        let mut err_out = Vec::new();
        let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
        let client = Client::initial().expect("creating client");

        let sdk = env.context.get_sdk().await.expect("Loading configured sdk");
        update_metadata_all(&sdk, output_dir.path(), &AuthFlowChoice::Default, &ui, &client)
            .await
            .expect("updating metadata");
        let urls = product_bundle_urls(&sdk).await.expect("get pb urls");
        assert!(!urls.is_empty());
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

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_v1() {
        let env = ffx_config::test_init().await.expect("create test config");

        let sdk_root_dir = create_test_intree_sdk();
        let sdk_root = sdk_root_dir.path().to_str().expect("path to str");
        ffx_config::query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.into())
            .await
            .expect("set sdk root path");
        ffx_config::query("sdk.type")
            .level(Some(ConfigLevel::User))
            .set("in-tree".into())
            .await
            .expect("set sdk type");
        ffx_config::query(CONFIG_METADATA)
            .level(Some(ConfigLevel::User))
            .set(serde_json::json!(["{sdk.root}/*.json"]))
            .await
            .expect("set pbms metadata");

        let sdk = env.context.get_sdk().await.expect("Loading configured sdk");

        let pb = load_product_bundle(&sdk, &None, ListingMode::ReadyBundlesOnly)
            .await
            .expect("could not load product bundle");
        assert_eq!(pb.loaded_from_path(), Utf8Path::new(sdk_root));
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

    //TODO(b/288891258) Fix the flakiness of these tests
    #[ignore]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_v2_valid() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "fake.x64");

        let env = ffx_config::test_init().await.expect("create test config");
        let sdk = env.context.get_sdk().await.expect("Loading configured sdk");

        // Load with passing a path directly
        let pb =
            load_product_bundle(&sdk, &Some(pb_dir.to_string()), ListingMode::ReadyBundlesOnly)
                .await
                .expect("could not load product bundle");
        assert_eq!(pb.loaded_from_path(), pb_dir);

        // Load with the config set with absolute path
        ffx_config::query(PRODUCT_BUNDLE_PATH_KEY)
            .level(Some(ConfigLevel::User))
            .set(serde_json::Value::String(pb_dir.to_string()))
            .await
            .expect("set product.path path");
        let pb = load_product_bundle(&sdk, &None, ListingMode::ReadyBundlesOnly)
            .await
            .expect("could not load product bundle");
        assert_eq!(pb.loaded_from_path(), pb_dir);
    }

    //TODO(b/288891258) Fix the flakiness of these tests
    #[ignore]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_load_product_bundle_v2_invalid() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = Utf8Path::from_path(tmp.path()).unwrap();
        let env = ffx_config::test_init().await.expect("create test config");
        let sdk = env.context.get_sdk().await.expect("Loading configured sdk");

        // Load with passing a path directly
        let pb =
            load_product_bundle(&sdk, &Some(pb_dir.to_string()), ListingMode::ReadyBundlesOnly)
                .await;
        assert!(pb.is_err());
    }
}
