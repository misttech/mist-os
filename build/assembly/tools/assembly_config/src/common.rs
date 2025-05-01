// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use camino::Utf8PathBuf;

use std::fs;

/// Return the "version" string if it is provided.
/// If not, return the contents of the file located at the path "version_file".
/// If neither argument is provided, return the string "unversioned".
pub fn get_release_version(
    version: &Option<String>,
    version_file: &Option<Utf8PathBuf>,
) -> Result<String> {
    Ok(match (version, version_file) {
        (None, None) => "unversioned".to_string(),
        (Some(_), Some(_)) => bail!("version and version_file cannot both be supplied"),
        (Some(version), _) => version.to_string(),
        (None, Some(version_file)) => {
            let s = fs::read_to_string(version_file)?;
            s.trim().to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::io::Write;
    use tempfile;

    #[test]
    fn test_default() {
        let version: Option<String> = None;
        let version_file: Option<Utf8PathBuf> = None;
        let version = get_release_version(&version, &version_file).unwrap();
        assert_eq!("unversioned".to_string(), version);
    }

    #[test]
    fn test_version_string() {
        let version: Option<String> = Some("version_string".to_string());
        let version_file: Option<Utf8PathBuf> = None;
        let version = get_release_version(&version, &version_file).unwrap();
        assert_eq!("version_string".to_string(), version);
    }

    #[test]
    fn test_version_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(&mut file, "version_file").unwrap();

        let version: Option<String> = None;
        let version_file: Option<Utf8PathBuf> =
            Some(Utf8Path::from_path(file.path()).unwrap().into());
        let version = get_release_version(&version, &version_file).unwrap();
        assert_eq!("version_file".to_string(), version);
    }

    #[test]
    fn test_error_for_both_versions() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(&mut file, "version_file").unwrap();

        let version: Option<String> = Some("version_string".to_string());
        let version_file: Option<Utf8PathBuf> =
            Some(Utf8Path::from_path(file.path()).unwrap().into());
        assert!(get_release_version(&version, &version_file).is_err());
    }

    #[test]
    fn test_version_file_missing() {
        let version: Option<String> = None;
        let version_file: Option<Utf8PathBuf> = Some(Utf8PathBuf::new());
        assert!(get_release_version(&version, &version_file).is_err());
    }
}
