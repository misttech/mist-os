// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Access utilities for gcs metadata.

use {
    crate::AuthFlowChoice,
    anyhow::{bail, Context, Result},
    errors::ffx_bail,
    gcs::{
        auth,
        client::{Client, DirectoryProgress, FileProgress, ProgressResponse, ProgressResult},
        error::GcsError,
        gs_url::split_gs_url,
        token_store::{read_boto_refresh_token, write_boto_refresh_token},
    },
    std::path::{Path, PathBuf},
    structured_ui,
};

/// Returns the path to the .boto (gsutil) configuration file.
pub(crate) async fn get_boto_path<I>(auth_flow: &AuthFlowChoice, ui: &I) -> Result<Option<PathBuf>>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("get_boto_path");
    if let AuthFlowChoice::Exec(_) = auth_flow {
        // The .boto file is not used for exec.
        return Ok(None);
    }
    // TODO(fxb/89584): Change to using ffx client Id and consent screen.
    let boto: Option<PathBuf> =
        ffx_config::get("flash.gcs.token").await.context("getting flash.gcs.token config value")?;
    match &boto {
        Some(boto_path) => {
            if !boto_path.is_file() {
                tracing::debug!("missing boto file at {:?}", boto_path);
                update_refresh_token(&boto_path, auth_flow, ui)
                    .await
                    .context("Set up refresh token")?
            }
        }
        None => ffx_bail!(
            "GCS authentication configuration value \"flash.gcs.token\" not \
            found. Set this value by running `ffx config set flash.gcs.token <path>` \
            to the path of the .boto file."
        ),
    };

    Ok(boto)
}

/// Return true if the blob is available.
///
/// `gcs_url` is the full GCS url, e.g. "gs://bucket/path/to/file".
/// The resulting data will be written to a directory at `local_dir`.
pub(crate) async fn exists_in_gcs<I>(
    gcs_url: &str,
    auth_flow: &AuthFlowChoice,
    ui: &I,
    client: &Client,
) -> Result<bool>
where
    I: structured_ui::Interface + Sync,
{
    let (gcs_bucket, gcs_path) = split_gs_url(gcs_url).context("Splitting gs URL.")?;
    loop {
        match client.exists(gcs_bucket, gcs_path).await {
            Ok(exists) => return Ok(exists),
            Err(e) => match e.downcast_ref::<GcsError>() {
                Some(GcsError::NeedNewAccessToken) => {
                    tracing::debug!("exists_in_gcs got NeedNewRefreshToken");
                    let access_token = handle_new_access_token(auth_flow, ui).await?;
                    client.set_access_token(access_token).await;
                }
                Some(GcsError::NotFound(_, _)) => {
                    // Ok(false) should be returned rather than NotFound.
                    unreachable!();
                }
                Some(_) | None => bail!(
                    "Cannot get product bundle container while \
                    downloading from gs://{}/{}, error {:?}",
                    gcs_bucket,
                    gcs_path,
                    e,
                ),
            },
        }
    }
}

/// Download from a given `gcs_url`.
///
/// `gcs_url` is the full GCS url, e.g. "gs://bucket/path/to/file".
/// The resulting data will be written to a directory at `local_dir`.
pub(crate) async fn fetch_from_gcs<F, I>(
    gcs_url: &str,
    local_dir: &Path,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    F: Fn(DirectoryProgress<'_>, FileProgress<'_>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("fetch_from_gcs {:?}", gcs_url);
    let (gcs_bucket, gcs_path) = split_gs_url(gcs_url).context("Splitting gs URL.")?;
    loop {
        tracing::debug!("gcs_bucket {:?}, gcs_path {:?}", gcs_bucket, gcs_path);
        match client
            .fetch_all(gcs_bucket, gcs_path, &local_dir, progress)
            .await
            .context("fetching all")
        {
            Ok(()) => break,
            Err(e) => match e.downcast_ref::<GcsError>() {
                Some(GcsError::NeedNewAccessToken) => {
                    tracing::debug!("fetch_from_gcs got NeedNewAccessToken");
                    let access_token = handle_new_access_token(auth_flow, ui).await?;
                    client.set_access_token(access_token).await;
                }
                Some(GcsError::NotFound(b, p)) => {
                    tracing::warn!("[gs://{}/{} not found]", b, p);
                    break;
                }
                Some(_) | None => bail!(
                    "Cannot get data from gs://{}/{}, saving to {:?}, error {:?}",
                    gcs_bucket,
                    gcs_path,
                    local_dir,
                    e,
                ),
            },
        }
    }
    Ok(())
}

/// Get a new access token based on the AuthFlowChoice.
///
/// Intended to simplify handling of a GcsError::NeedNewAccessToken error.
pub async fn handle_new_access_token<I>(auth_flow: &AuthFlowChoice, ui: &I) -> Result<String>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("handle_new_access_token");
    let access_token = match auth_flow {
        AuthFlowChoice::Default
        | AuthFlowChoice::Pkce
        | AuthFlowChoice::Oob
        | AuthFlowChoice::Device => {
            let boto_path = get_boto_path(auth_flow, ui).await?;
            let path =
                boto_path.as_ref().expect("A .boto path is required. Please report as a bug.");
            let refresh_token = read_boto_refresh_token(path).context("reading boto refresh")?;
            let access_token = match auth::pkce::new_access_token(&refresh_token).await {
                Ok(a) => a,
                Err(GcsError::NeedNewRefreshToken) => {
                    update_refresh_token(&path, auth_flow, ui)
                        .await
                        .context("Updating refresh token")?;
                    // Make one additional attempt now that the refresh token
                    // is updated.
                    let refresh_token =
                        read_boto_refresh_token(path).context("reading boto refresh")?;
                    auth::pkce::new_access_token(&refresh_token).await?
                }
                Err(_) => bail!("Failed to get new access token"),
            };
            access_token
        }
        AuthFlowChoice::Exec(exec) => {
            let output = std::process::Command::new(exec).output()?;
            if !output.status.success() {
                tracing::error!(
                    "The {:?} process to get an access token returned {:?}.",
                    exec,
                    output.status.code()
                );
                return Err(GcsError::ExecForAccessFailed.into());
            }
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        AuthFlowChoice::NoAuth => return Err(GcsError::AuthRequired.into()),
    };
    Ok(access_token)
}

/// Download a single file from `gcs_url` to an in-ram string.
///
/// `gcs_url` is the full GCS url, e.g. "gs://bucket/path/to/file".
pub(crate) async fn string_from_gcs<F, I>(
    gcs_url: &str,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<String>
where
    F: Fn(FileProgress<'_>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("string_from_gcs {:?}", gcs_url);
    let (gcs_bucket, gcs_path) = split_gs_url(gcs_url).context("Splitting gs URL.")?;
    let mut result = Vec::new();
    loop {
        tracing::debug!("gcs_bucket {:?}, gcs_path {:?}", gcs_bucket, gcs_path);
        match client
            .write(gcs_bucket, gcs_path, &mut result, progress)
            .await
            .context("writing to string")
        {
            Ok(ProgressResponse::Continue) => break,
            Ok(ProgressResponse::Cancel) => {
                tracing::info!("ProgressResponse requesting cancel, exiting");
                std::process::exit(1);
            }
            Err(e) => match e.downcast_ref::<GcsError>() {
                Some(GcsError::NeedNewAccessToken) => {
                    tracing::debug!("string_from_gcs got NeedNewAccessToken");
                    let access_token = handle_new_access_token(auth_flow, ui).await?;
                    client.set_access_token(access_token).await;
                }
                Some(GcsError::NotFound(b, p)) => {
                    tracing::warn!("[gs://{}/{} not found]", b, p);
                    break;
                }
                Some(gcs_err) => bail!(
                    "Cannot get data from gs://{}/{} to string, error {:?}, {:?}",
                    gcs_bucket,
                    gcs_path,
                    e,
                    gcs_err,
                ),
                None => bail!(
                    "Cannot get data from gs://{}/{} to string (Non-GcsError), error {:?}",
                    gcs_bucket,
                    gcs_path,
                    e,
                ),
            },
        }
    }
    Ok(String::from_utf8_lossy(&result).to_string())
}

/// Prompt the user to visit the OAUTH2 permissions web page and enter a new
/// authorization code, then convert that to a refresh token and write that
/// refresh token to the ~/.boto file.
async fn update_refresh_token<I>(boto_path: &Path, auth_flow: &AuthFlowChoice, ui: &I) -> Result<()>
where
    I: structured_ui::Interface + Sync,
{
    tracing::debug!("update_refresh_token {:?}", boto_path);
    println!("\nThe refresh token in the {:?} file needs to be updated.", boto_path);
    let refresh_token = match auth_flow {
        AuthFlowChoice::Default | AuthFlowChoice::Pkce => {
            auth::pkce::new_refresh_token(ui).await.context("get refresh token")?
        }
        AuthFlowChoice::Oob => {
            auth::oob::new_refresh_token().await.context("get oob refresh token")?
        }
        AuthFlowChoice::Device => {
            auth::device::new_refresh_token(ui).await.context("get device refresh token")?
        }
        AuthFlowChoice::Exec(_) => {
            bail!("There's no refresh token used with an executable for auth.");
        }
        AuthFlowChoice::NoAuth => {
            bail!("The refresh token should not be updated when no-auth is used.");
        }
    };
    tracing::debug!("Writing boto file {:?}", boto_path);
    write_boto_refresh_token(boto_path, &refresh_token)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, tempfile::NamedTempFile};

    // TODO(fxbug.dev/92773): This test requires mocks for interactivity and
    // https. The test is currently disabled.
    #[ignore]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_update_refresh_token() {
        let temp_file = NamedTempFile::new().expect("temp file");
        let ui = structured_ui::MockUi::new();
        update_refresh_token(&temp_file.path(), &AuthFlowChoice::Default, &ui)
            .await
            .expect("set refresh token");
    }
}
