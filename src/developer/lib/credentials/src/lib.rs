// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Store and retrieve user (developer) credentials.
//!
//! When creating a tool which needs access to the developer's OAuth2 refresh
//! token, or when storing a new such token, use this lib to perform that
//! operation.
//!
//! Caution: Some data handled here are security access keys (tokens) and must
//!          not be logged (or traced) or otherwise put someplace where the
//!          secrecy could be compromised. I.e. watch out when adding/reviewing
//!          log::*, tracing::*, or `impl` of Display or Debug.

use {
    anyhow::{anyhow, Context, Result},
    gcs::auth::GcsCredentials,
    serde::{Deserialize, Serialize},
    std::{
        env, fmt,
        fs::{create_dir_all, set_permissions, Permissions},
        io::{BufWriter, Write},
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    },
};

mod boto;

const FILE_NAME: &str = "credentials.json";
const VERSION_1_LABEL: &str = "1";

/// We are using client id of gcloud app. This value can be gotten by `gcloud config get auth/client_id`
const BOTO_CLIENT_ID: &str = "32555940559.apps.googleusercontent.com";

/// For a web site, a client secret is kept locked away in a secure server. This
/// is not a web site and the value is needed, so a non-secret "secret" is used.
///
/// "Google OAuth2 clients always have a secret, even if the client is an
/// installed application/utility such as gsutil.  Of course, in such cases the
/// "secret" is actually publicly known; security depends entirely on the
/// secrecy of refresh tokens, which effectively become bearer tokens."
const BOTO_CLIENT_SECRET: &str = "ZmssLNjJy2998hD4CTg2ejr2";

const GCLOUD_PATH: &str = ".config/gcloud/application_default_credentials.json";

/// This format matches that of the gcloud application_default_credentials.json,
/// which eases the transition for zxdb and symbolizer. The format may evolved
/// after the transition.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct OAuth2Credentials {
    /// The OAuth2 client which requested data access on behalf of the user.
    #[serde(default)]
    pub client_id: String,

    /// The client "secret" for desktop applications are not actually secret.
    /// In this case it acts like additional information for the client_id.
    #[serde(default)]
    pub client_secret: String,

    /// A long-lived token which is used to mint access tokens. This is private
    /// to the user and must not be printed to a log or otherwise leaked.
    #[serde(default)]
    pub refresh_token: String,

    /// The type of this record (somewhat like a version), which is currently
    /// "authorized_user" to mimic the value used by gcloud.
    #[serde(default)]
    pub r#type: String,
}

impl Default for OAuth2Credentials {
    fn default() -> Self {
        // Use the values from gcs lib since that is where new credentials are
        // created. This allows the client ID/secret to be defined and updated
        // in one place.
        let default = GcsCredentials::new("");
        Self {
            client_id: default.client_id,
            client_secret: default.client_secret,
            refresh_token: default.refresh_token,
            r#type: "authorized_user".to_string(),
        }
    }
}

/// Custom debug to avoid printing the refresh_token.
impl fmt::Debug for OAuth2Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OAuth2 client_id: {:?}, client_secret: {:?}, refresh_token: <hidden>",
            self.client_id, self.client_secret
        )
    }
}

/// User (developer) credentials stored to disk.
#[derive(Default, Debug, Deserialize, PartialEq, Serialize)]
pub struct Credentials {
    /// The version of the file schema. Currently always VERSION_1_LABEL ("1").
    pub version: String,

    /// Credentials for Google OAuth2.
    #[serde(default)]
    pub oauth2: OAuth2Credentials,
}

impl Credentials {
    pub fn new() -> Self {
        Self { version: VERSION_1_LABEL.to_string(), ..Default::default() }
    }

    /// Read the existing credentials from the XDG_DATA_HOME, or create a new,
    /// empty set of credentials.
    pub async fn load_or_new() -> Self {
        let instance = if let Ok(instance) = load_from_home_data() {
            tracing::debug!("Load credential from home data");
            instance
        } else if let Ok(instance) = legacy_read().await {
            tracing::debug!("Get credential through legacy_read");
            instance
        } else {
            tracing::debug!("Create new credential");
            Credentials::new()
        };
        tracing::debug!("Loaded credentials version {}", instance.version);
        instance
    }

    /// Write the credentials to the XDG_DATA_HOME.
    pub async fn save(&self) -> Result<()> {
        let path = developer_data_path().context("building developer data path")?.join(FILE_NAME);
        let out_file = std::fs::File::create(&path)
            .with_context(|| format!("Unable to create file at {:?}", path))?;

        // Force the permissions to user only read+write.
        const USER_READ_WRITE: u32 = 0o600;
        set_permissions(&path, Permissions::from_mode(USER_READ_WRITE))?;

        let mut writer = BufWriter::new(out_file);
        serde_json::to_writer_pretty(&mut writer, &self)
            .with_context(|| format!("writing json to {:?}", path))?;
        writer.flush()?;
        tracing::debug!("Saved credentials version {}", self.version);
        Ok(())
    }

    /// Get the OAuth2 credentials as a GCS separate struct (handy for working
    /// with the gcs lib).
    pub fn gcs_credentials(&self) -> GcsCredentials {
        GcsCredentials {
            client_id: self.oauth2.client_id.clone(),
            client_secret: self.oauth2.client_secret.clone(),
            refresh_token: self.oauth2.refresh_token.clone(),
        }
    }
}

/// Get the oauth2 refresh token from the credentials file.
///
/// This is a handy wrapper for creating a Credentials object. It also handles
/// the soft-transition from using boto files.
///
/// Alert: The refresh token is considered a private secret for the user. Do
///        not print the token to a log or otherwise disclose it.
async fn legacy_read() -> Result<Credentials> {
    // TODO(https://fxbug.dev/89584): Change to using ffx client Id and consent screen.
    let mut credentials = Credentials::new();
    use home::home_dir;
    let boto_path = if let Ok(Some(boto_path)) =
        ffx_config::query("flash.gcs.token").get_file::<Option<PathBuf>>().await
    {
        tracing::debug!("legacy_read: trying flash.gcs.token at {:?}", boto_path);
        boto_path
    } else {
        tracing::debug!("legacy_read: trying $HOME/.boto");
        Path::new(&home_dir().expect("getting home dir")).join(".boto")
    };
    if let Ok(token) = boto::read_boto_refresh_token(&boto_path) {
        // Do not use the client ID/secret from the gcs lib because those may
        // change and this is explicitly about the .boto file (so it should not
        // change).
        credentials.oauth2.client_id = BOTO_CLIENT_ID.to_string();
        credentials.oauth2.client_secret = BOTO_CLIENT_SECRET.to_string();
        credentials.oauth2.refresh_token = token.to_string();
        credentials.save().await?;
        return Ok(credentials);
    }
    tracing::debug!("legacy_read: trying gcloud at $HOME/{}", GCLOUD_PATH);
    let path =
        Path::new(&home_dir().expect("getting application_default_credentials")).join(GCLOUD_PATH);
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(oauth) = serde_json::from_str::<OAuth2Credentials>(&data) {
            credentials.oauth2 = oauth.clone();
            credentials.save().await?;
            return Ok(credentials);
        }
    }
    Err(anyhow!("Unable to find legacy credentials"))
}

fn load_from_home_data() -> Result<Credentials> {
    let path = developer_data_path()?.join(FILE_NAME);
    let in_file = std::fs::File::open(&path)?;
    Ok(serde_json::from_reader(in_file)?)
}

fn data_base_path() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        Ok(home)
    } else {
        env::var("XDG_DATA_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

/// Get the path "{base_path_for_xdg_data}/Fuchsia/developer".
fn developer_data_path() -> Result<PathBuf> {
    let mut path = data_base_path()?;
    path.push("Fuchsia");
    path.push("developer");
    create_dir_all(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use {super::*, serial_test::serial, temp_test_env::TempTestEnv};

    #[test]
    fn test_new() {
        let test = Credentials::new();
        assert_eq!(test.version, VERSION_1_LABEL);
        assert_eq!(test.oauth2.r#type, "authorized_user".to_string());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[serial]
    async fn test_load_and_save() {
        let _test_env = TempTestEnv::new().expect("test env");
        let mut test = Credentials::load_or_new().await;
        assert_eq!(test.version, VERSION_1_LABEL);
        assert_eq!(test.oauth2.r#type, "authorized_user".to_string());
        test.oauth2 = OAuth2Credentials {
            client_id: "fake_id".to_string(),
            client_secret: "fake_secret".to_string(),
            refresh_token: "fake_token".to_string(),
            r#type: "fake_type".to_string(),
        };
        assert_eq!(test.oauth2.refresh_token, "fake_token".to_string());
        test.save().await.expect("saving test credentials");
        assert_eq!(test.oauth2.refresh_token, "fake_token".to_string());
        drop(test);
        let mut test2 = Credentials::load_or_new().await;
        assert_eq!(test2.version, VERSION_1_LABEL);
        assert_eq!(test2.oauth2.refresh_token, "fake_token".to_string());
        assert_eq!(test2.oauth2.client_id, "fake_id".to_string());
        assert_eq!(test2.oauth2.client_secret, "fake_secret".to_string());
        assert_eq!(test2.oauth2.r#type, "fake_type".to_string());
        test2.version = "incorrect_version".to_string();
        test2.save().await.expect("saving test2 credentials");
    }
}
