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
    serde::{Deserialize, Serialize},
    std::{
        env, fmt,
        fs::{create_dir_all, set_permissions, Permissions},
        io::{BufWriter, Write},
        os::unix::fs::PermissionsExt,
        path::PathBuf,
    },
};

const FILE_NAME: &str = "credentials.json";
const VERSION_1_LABEL: &str = "1";

#[derive(Deserialize, PartialEq, Serialize)]
pub struct OAuth2 {
    /// The OAuth2 client which requested data access on behalf of the user.
    #[serde(default)]
    pub client_id: Option<String>,

    /// The client "secret" for desktop applications are not actually secret.
    /// In this case it acts like additional information for the client_id.
    #[serde(default)]
    pub client_secret: Option<String>,

    /// A long-lived token which is used to mint access tokens. This is private
    /// to the user and must not be printed to a log or otherwise leaked.
    #[serde(default)]
    pub refresh_token: String,
}

/// Custom debug to avoid printing the refresh_token.
impl fmt::Debug for OAuth2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OAuth2 client_id: {:?}, client_secret: {:?}, refresh_token: <hidden>",
            self.client_id, self.client_secret
        )
    }
}

/// User (developer) credentials stored to disk.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Credentials {
    /// The version of the file schema. Currently always VERSION_1_LABEL ("1").
    pub version: String,

    /// Credentials for Google OAuth2.
    #[serde(default)]
    pub oauth2: Option<OAuth2>,
}

impl Credentials {
    pub fn new() -> Self {
        Self { version: VERSION_1_LABEL.to_string(), oauth2: None }
    }

    /// Read the existing credentials from the XDG_DATA_HOME, or create a new,
    /// empty set of credentials.
    pub fn load_or_new() -> Self {
        let instance = load_from_home_data()
            .or_else(|e| -> Result<Self> {
                tracing::info!("Error loading credentials {:?}. Creating new.", e);
                Ok(Credentials::new())
            })
            .unwrap();
        tracing::debug!("Loaded credentials version {}", instance.version);
        instance
    }

    /// Write the credentials to the XDG_DATA_HOME.
    pub fn save(&self) -> Result<()> {
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
        assert_eq!(test.oauth2, None);
    }

    #[test]
    #[serial]
    fn test_load_and_save() {
        let _test_env = TempTestEnv::new().expect("test env");
        let mut test = Credentials::load_or_new();
        assert_eq!(test.version, VERSION_1_LABEL);
        assert_eq!(test.oauth2, None);
        test.oauth2 = Some(OAuth2 {
            client_id: None,
            client_secret: None,
            refresh_token: "fake".to_string(),
        });
        assert_eq!(test.oauth2.as_ref().unwrap().refresh_token, "fake".to_string());
        test.save().expect("saving test credentials");
        assert_eq!(test.oauth2.as_ref().unwrap().refresh_token, "fake".to_string());
        drop(test);
        let mut test2 = Credentials::load_or_new();
        assert_eq!(test2.version, VERSION_1_LABEL);
        assert_eq!(test2.oauth2.as_ref().unwrap().refresh_token, "fake".to_string());
        test2.version = "incorrect_version".to_string();
        test2.save().expect("saving test2 credentials");
    }
}
