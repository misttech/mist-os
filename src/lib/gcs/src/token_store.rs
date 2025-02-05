// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provide Google Cloud Storage (GCS) access.

use crate::error::GcsError;
use crate::exponential_backoff::default_backoff_strategy;
use anyhow::{bail, Context, Result};
use async_lock::Mutex;
use fuchsia_backoff::retry_or_last_error;
use fuchsia_hyper::HttpsClient;
use http::{request, StatusCode};
use hyper::{Body, Method, Request, Response};
use std::fmt;
use url::Url;

/// Base URL for JSON API access.
const API_BASE: &str = "https://www.googleapis.com/storage/v1";

/// Base URL for reading (blob) objects.
const STORAGE_BASE: &str = "https://storage.googleapis.com";

/// Response from the `/b/<bucket>/o` object listing API.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResponse {
    /// Continuation token; only present when there is more data.
    next_page_token: Option<String>,

    /// List of objects, sorted by name.
    #[serde(default)]
    items: Vec<ListResponseItem>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResponseItem {
    /// GCS object name.
    name: String,
}

/// User credentials for use with GCS.
///
/// Specifically to:
/// - api_base: https://www.googleapis.com/storage/v1
/// - storage_base: https://storage.googleapis.com
pub struct TokenStore {
    /// Base URL for JSON API access.
    api_base: Url,

    /// Base URL for reading (blob) objects.
    storage_base: Url,

    /// A limited time token used in an Authorization header.
    pub(crate) access_token: Mutex<String>,
}

impl TokenStore {
    /// Allow access to public and private (auth-required) GCS data.
    pub fn new() -> Result<Self, GcsError> {
        Ok(Self {
            api_base: Url::parse(API_BASE).expect("parse API_BASE"),
            storage_base: Url::parse(STORAGE_BASE).expect("parse STORAGE_BASE"),
            access_token: Mutex::new("".to_string()),
        })
    }

    /// Allow access to public and private (auth-required) GCS data.
    pub fn new_with_urls(api_base: &str, storage_base: &str) -> Result<Self, GcsError> {
        Ok(Self {
            api_base: Url::parse(api_base).expect("parse api_base"),
            storage_base: Url::parse(storage_base).expect("parse storage_base"),
            access_token: Mutex::new("".to_string()),
        })
    }

    /// Create localhost base URLs and fake credentials for testing.
    #[cfg(test)]
    fn local_fake() -> Self {
        let api_base = Url::parse("http://localhost:9000").expect("api_base");
        let storage_base = Url::parse("http://localhost:9001").expect("storage_base");
        Self { api_base, storage_base, access_token: Mutex::new("".to_string()) }
    }

    /// Create a new https client with shared access to the GCS credentials.
    ///
    /// Multiple clients may be created to perform downloads in parallel.
    pub async fn set_access_token(&self, access: String) {
        let mut access_token = self.access_token.lock().await;
        *access_token = access;
    }

    /// Apply Authorization header, if available and uri is https.
    ///
    /// IF the access_token is empty OR
    /// IF the URI is not already set to an "https" uri
    /// THEN no changes are made to the builder.
    async fn maybe_authorize(&self, builder: request::Builder) -> Result<request::Builder> {
        let access_token = self.access_token.lock().await;
        if !access_token.is_empty() {
            // Passing the access token over a non-secure path is forbidden.
            if builder.uri_ref().and_then(|x| x.scheme()).map(|x| x.as_str()) == Some("https") {
                log::debug!("maybe_authorize: adding Bearer Authorization");
                return Ok(builder.header("Authorization", format!("Bearer {}", access_token)));
            }
        }
        Ok(builder)
    }

    /// Reads content of a stored object (blob) from GCS.
    ///
    /// A leading slash "/" on `object` will be ignored.
    pub(crate) async fn download(
        &self,
        https_client: &HttpsClient,
        bucket: &str,
        object: &str,
    ) -> Result<Response<Body>> {
        log::debug!("download {:?}, {:?}", bucket, object);
        // If the bucket and object are from a gs:// URL, the object may have a
        // undesirable leading slash. Trim it if present.
        let object = if object.starts_with('/') { &object[1..] } else { object };

        let url = self
            .storage_base
            .join(&format!("{}/{}", bucket, object))
            .context("joining to storage base")?;
        self.request_with_retries(https_client, url).await.context("sending http(s) request")
    }

    /// Make one attempt to request data from GCS.
    ///
    /// Callers are expected to handle errors and call send_request() again as
    /// desired (e.g. follow redirects).
    async fn send_request(&self, https_client: &HttpsClient, url: Url) -> Result<Response<Body>> {
        log::debug!("https_client.request {:?}", url);
        let req = Request::builder().method(Method::GET).uri::<String>(url.into());
        let req = self.maybe_authorize(req).await.context("authorizing in send_request")?;
        let req = req.body(Body::empty()).context("creating request body")?;
        let auth_used = req.headers().contains_key("Authorization");

        let res = https_client.request(req).await.context("https_client.request")?;
        match res.status() {
            // Status 403 (FORBIDDEN) means an access token is needed.
            // If an access token was already used, there's no need in getting
            // a new one, the server is saying NO to this request.
            StatusCode::FORBIDDEN if !auth_used => {
                log::debug!("send_request status {} (FORBIDDEN)", res.status());
                bail!(GcsError::NeedNewAccessToken);
            }
            // Status 401 (UNAUTHORIZED) means the access token didn't work.
            StatusCode::UNAUTHORIZED => {
                log::debug!("send_request status {} (UNAUTHORIZED)", res.status());
                bail!(GcsError::NeedNewAccessToken);
            }
            _ => (),
        }
        Ok(res)
    }

    /// Request data from GCS, retrying in the case of common transient errors.
    ///
    /// Callers are expected to handle non-transient errors and call
    /// request_with_retries() again as desired (e.g. follow redirects).
    /// See https://cloud.google.com/storage/docs/retry-strategy.
    async fn request_with_retries(
        &self,
        https_client: &HttpsClient,
        url: Url,
    ) -> Result<Response<Body>> {
        retry_or_last_error(default_backoff_strategy(), || async {
            let result =
                self.send_request(https_client, url.clone()).await.context("send_request")?;
            let status = result.status();
            // Retry on http errors 408 | 429 | 500..=599.
            if matches!(status, StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS)
                || status.is_server_error()
            {
                bail!(GcsError::HttpTransientError(result.status()))
            }
            Ok(result)
        })
        .await
        .context("send_request with retries")
    }

    /// Determine whether a gs url points to either a file or directory.
    ///
    /// A leading slash "/" on `prefix` will be ignored.
    ///
    /// Ok(false) will be returned instead of GcsError::NotFound.
    pub async fn exists(
        &self,
        https_client: &HttpsClient,
        bucket: &str,
        prefix: &str,
    ) -> Result<bool> {
        log::debug!("testing existence of gs://{}/{}", bucket, prefix);
        // Note: gs 'stat' will not match a directory.  So, gs 'list' is used to
        // determine existence. The number of items in a directory may be
        // enormous, so the results are limited to one item.
        match self.list(https_client, bucket, prefix, /*limit=*/ Some(1)).await {
            Ok(list) => {
                assert!(list.len() <= 1, "exists returned {} items.", list.len());
                Ok(!list.is_empty())
            }
            Err(e) => match e.downcast_ref::<GcsError>() {
                Some(GcsError::NotFound(_, _)) => Ok(false),
                Some(_) | None => Err(e),
            },
        }
    }

    /// List objects from GCS in `bucket` with matching `prefix`.
    ///
    /// A leading slash "/" on `prefix` will be ignored.
    pub async fn list(
        &self,
        https_client: &HttpsClient,
        bucket: &str,
        prefix: &str,
        limit: Option<u32>,
    ) -> Result<Vec<String>> {
        log::debug!("list objects at gs://{}/{}", bucket, prefix);
        self.attempt_list(https_client, bucket, prefix, limit).await
    }

    /// Make one attempt to list objects from GCS.
    ///
    /// If `limit` is given, at most N results will be returned. If `limit` is
    /// None then all matching values will be returned.
    async fn attempt_list(
        &self,
        https_client: &HttpsClient,
        bucket: &str,
        prefix: &str,
        limit: Option<u32>,
    ) -> Result<Vec<String>> {
        log::debug!("attempt_list of gs://{}/{}", bucket, prefix);
        // If the bucket and prefix are from a gs:// URL, the prefix may have a
        // undesirable leading slash. Trim it if present.
        let prefix = if prefix.starts_with('/') { &prefix[1..] } else { prefix };

        let mut base_url = self.api_base.to_owned();
        base_url.path_segments_mut().unwrap().extend(&["b", bucket, "o"]);
        base_url
            .query_pairs_mut()
            .append_pair("prefix", prefix)
            .append_pair("prettyPrint", "false")
            .append_pair("fields", "nextPageToken,items/name");
        if let Some(limit) = limit {
            base_url.query_pairs_mut().append_pair("maxResults", &limit.to_string());
        }
        let mut results = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            // Create a new URL for each "page" of results.
            let mut url = base_url.clone();
            if let Some(t) = page_token {
                url.query_pairs_mut().append_pair("pageToken", t.as_str());
            }
            let res =
                self.request_with_retries(https_client, url).await.context("sending request")?;
            match res.status() {
                StatusCode::OK => {
                    let bytes = hyper::body::to_bytes(res.into_body())
                        .await
                        .context("hyper::body::to_bytes")?;
                    let info: ListResponse =
                        serde_json::from_slice(&bytes).context("serde_json::from_slice")?;
                    results.extend(info.items.into_iter().map(|i| i.name));
                    if info.next_page_token.is_none() {
                        break;
                    }
                    if let Some(limit) = limit {
                        if results.len() >= limit as usize {
                            break;
                        }
                    }
                    page_token = info.next_page_token;
                }
                _ => {
                    bail!("Failed to list {:?} {:?}", base_url, res);
                }
            }
        }
        if results.is_empty() {
            bail!(GcsError::NotFound(bucket.to_string(), prefix.to_string()));
        }
        Ok(results)
    }
}

impl fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenStore")
            .field("api_base", &self.api_base)
            .field("storage_base", &self.storage_base)
            .field("access_token", &"<hidden>")
            .finish()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_hyper::new_https_client;

    #[should_panic(expected = "Connection refused")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_fake_download() {
        let token_store = TokenStore::local_fake();
        let bucket = "fake_bucket";
        let object = "fake/object/path.txt";
        token_store.download(&new_https_client(), bucket, object).await.expect("client download");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_maybe_authorize() {
        let store = TokenStore::new().expect("creating token store");
        let req = Request::builder().method(Method::GET).uri("http://example.com/".to_string());
        let req = store.maybe_authorize(req).await.expect("maybe_authorize");
        let headers = req.headers_ref().unwrap();
        // The authorization is not set because the access token is empty.
        assert!(!headers.contains_key("Authorization"));

        store.set_access_token("fake_token".to_string()).await;
        let req = Request::builder().method(Method::GET).uri("http://example.com/".to_string());
        let req = store.maybe_authorize(req).await.expect("maybe_authorize");
        let headers = req.headers_ref().unwrap();
        // The authorization is not set because the uri is not https.
        assert!(!headers.contains_key("Authorization"));

        let req = Request::builder().method(Method::GET).uri("https://example.com/".to_string());
        let req = store.maybe_authorize(req).await.expect("maybe_authorize");
        let headers = req.headers_ref().unwrap();
        // The authorization is set because there is a token and an https url.
        assert_eq!(headers["Authorization"], "Bearer fake_token");
    }

    // This test is marked "ignore" because it actually downloads from GCS,
    // which isn't good for a CI/GI test. It's here because it's handy to have
    // as a local developer test. Run with `fx test gcs_lib_test -- --ignored`.
    // Note: gsutil config is required.
    #[ignore]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_gcs_download_public() {
        let token_store = TokenStore::new().expect("creating token store");
        let bucket = "fuchsia";
        let object = "development/5.20210610.3.1/sdk/linux-amd64/gn.tar.gz";
        let res = token_store
            .download(&new_https_client(), bucket, object)
            .await
            .expect("client download");
        assert_eq!(res.status(), StatusCode::OK);
    }
}
