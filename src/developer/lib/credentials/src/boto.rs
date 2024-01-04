// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Retrieve user credentials from the boto file used by gsutil.
//!
//! This is for transitioning to using the credentials file only and should be
//! deleted at some point.

use {
    anyhow::{bail, Context, Result},
    once_cell::sync::OnceCell,
    regex::Regex,
    std::{fs, path::Path, string::String},
};

/// Fetch an existing refresh token from a .boto (gsutil) configuration file.
///
/// Tip, the `boto_path` is commonly "~/.boto". E.g.
/// ```
/// use home::home_dir;
/// let boto_path = Path::new(&home_dir().expect("home dir")).join(".boto");
/// ```
///
/// TODO(https://fxbug.dev/82014): Using an ffx specific token will be preferred once
/// that feature is created. For the near term, an existing gsutil token is
/// workable.
///
/// Alert: The refresh token is considered a private secret for the user. Do
///        not print the token to a log or otherwise disclose it.
pub(crate) fn read_boto_refresh_token<P>(boto_path: P) -> Result<String>
where
    P: AsRef<Path> + std::fmt::Debug,
{
    // Read the file at `boto_path` to retrieve a value from a line resembling
    // "gs_oauth2_refresh_token = <string_of_chars>".
    static GS_REFRESH_TOKEN_RE: OnceCell<Regex> = OnceCell::new();
    let re = GS_REFRESH_TOKEN_RE
        .get_or_init(|| Regex::new(r#"\n\s*gs_oauth2_refresh_token\s*=\s*(\S+)"#).expect("regex"));
    let data = fs::read_to_string(boto_path.as_ref()).context("read_to_string boto_path")?;
    let refresh_token = match re.captures(&data) {
        Some(found) => found.get(1).expect("found at least one").as_str().to_string(),
        None => bail!(
            "A gs_oauth2_refresh_token entry was not found in {:?}. \
            Please check that the file is writable, that there's available \
            disk space, and authenticate again",
            boto_path
        ),
    };
    Ok(refresh_token)
}

#[cfg(test)]
mod test {
    use {super::*, serial_test::serial, tempfile};

    /// Overwrite the 'gs_oauth2_refresh_token' in the file at `boto_path`.
    ///
    /// TODO(https://fxbug.dev/82014): Using an ffx specific token will be preferred once
    /// that feature is created. For the near term, an existing gsutil token is
    /// workable.
    ///
    /// Alert: The refresh token is considered a private secret for the user. Do
    ///        not print the token to a log or otherwise disclose it.
    pub fn write_boto_refresh_token<P: AsRef<Path>>(boto_path: P, token: &str) -> Result<()> {
        use std::{fs::set_permissions, os::unix::fs::PermissionsExt};
        let boto_path = boto_path.as_ref();
        let data = if !boto_path.is_file() {
            fs::File::create(boto_path).context("Create .boto file")?;
            const USER_READ_WRITE: u32 = 0o600;
            let permissions = std::fs::Permissions::from_mode(USER_READ_WRITE);
            set_permissions(&boto_path, permissions).context("Boto set permissions")?;
            format!(
                "# This file was created by the Fuchsia GCS lib.\
                \n[GSUtil]\
                \ngs_oauth2_refresh_token = {}\
                \ndefault_project_id =\
                \n",
                token
            )
        } else {
            static GS_UPDATE_REFRESH_TOKEN_RE: OnceCell<Regex> = OnceCell::new();
            let re = GS_UPDATE_REFRESH_TOKEN_RE.get_or_init(|| {
                Regex::new(r#"(\n\s*gs_oauth2_refresh_token\s*=\s*)\S*"#).expect("regex")
            });
            let data = fs::read_to_string(boto_path).context("replace boto refresh")?;
            re.replace(&data, format!("${{1}}{}", token).as_str()).to_string()
        };
        fs::write(boto_path, data).context("Writing .boto file")?;
        Ok(())
    }

    #[test]
    #[serial]
    fn test_gcs_read_refresh_token() {
        let boto_temp = tempfile::TempDir::new().expect("temp dir");
        let boto_path = boto_temp.path().join(".boto");
        assert!(!boto_path.is_file());
        // Test with no .boto file.
        write_boto_refresh_token(&boto_path, "first-token").expect("write token");
        assert!(boto_path.is_file());
        let refresh_token = read_boto_refresh_token(&boto_path).expect("first token");
        assert_eq!(refresh_token, "first-token".to_string());
        // Test updating existing .boto file.
        write_boto_refresh_token(&boto_path, "second-token").expect("write token");
        let refresh_token = read_boto_refresh_token(&boto_path).expect("second token");
        assert_eq!(refresh_token, "second-token".to_string());
    }
}
