// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fs::File, io::ErrorKind};

use camino::{Utf8Path, Utf8PathBuf};

use crate::{
    fuchsia_env::{FuchsiaEnv, ParseError},
    ConfigPath,
};

pub const FILE_STEM: &str = "fuchsia_env";
pub const TOML_FILE_EXTENSION: &str = "toml";
pub const JSON5_FILE_EXTENSION: &str = "json5";
pub const FILE_EXTENSIONS: &[&str] = &[TOML_FILE_EXTENSION, JSON5_FILE_EXTENSION];

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum LoadError {
    #[error("Multiple candidate files were present (also found `{0}`), remove one of them to resolve the ambiguity")]
    MultipleFiles(Utf8PathBuf),
    #[error(transparent)]
    Parsing(#[from] ParseError),
    #[error("Couldn't determine canonical project root path")]
    PathError(std::io::Error),
    #[error("Error reading file")]
    Io(#[from] std::io::Error),
}

#[derive(thiserror::Error, Debug)]
#[error("Loading {path}")]
#[non_exhaustive]
pub struct FileError {
    path: Utf8PathBuf,
    #[source]
    kind: LoadError,
}

impl FileError {
    /// Helper function to generate a closure that can be passed to
    /// [`Result::map_err`] to connect the path with the actual error.
    fn with_path<T>(path: &Utf8Path, inner: T) -> Self
    where
        LoadError: From<T>,
    {
        let path = path.to_owned();
        let kind = LoadError::from(inner);
        Self { path, kind }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigDomain {
    root: Utf8PathBuf,
    path: Utf8PathBuf,
    build_dir: Option<Utf8PathBuf>,
    build_config_file: Option<Utf8PathBuf>,
    sdk_root: Option<Utf8PathBuf>,
    contents: FuchsiaEnv,
}

impl ConfigDomain {
    /// Finds the root of a fuchsia configuration domain by searching parent
    /// directories until it finds one that contains a file that matches the
    /// right pattern (a filename stem of [`Self::FILE_STEM`] and an extension
    /// in [`Self::FILE_EXTENSIONS`]). If one is found, it will return the path
    /// to the file it found.
    pub fn find_root(mut from_dir: &Utf8Path) -> Option<Utf8PathBuf> {
        loop {
            for ext in FILE_EXTENSIONS {
                let check_path = from_dir.join(format!("{FILE_STEM}.{ext}"));
                if check_path.exists() {
                    return Some(check_path);
                }
            }
            from_dir = from_dir.parent()?;
        }
    }

    /// Loads the fuchsia_env file with the format indicated by its extension.
    pub fn load_from(path: &Utf8Path) -> Result<Self, FileError> {
        let loading_extension = check_adjacent_files(path)?;

        let file = File::open(path).map_err(|e| FileError::with_path(path, e))?;

        let contents = match loading_extension {
            TOML_FILE_EXTENSION => {
                FuchsiaEnv::load_toml(file).map_err(|e| FileError::with_path(path, e))
            }
            JSON5_FILE_EXTENSION => {
                FuchsiaEnv::load_json5(file).map_err(|e| FileError::with_path(path, e))
            }
            _ => Err(FileError::with_path(
                path,
                ParseError::UnknownFormat(loading_extension.to_owned()),
            )),
        }?;
        let path = path
            .canonicalize_utf8()
            .map_err(|e| FileError::with_path(path, LoadError::PathError(e)))?;
        let root = path
            .parent()
            .ok_or_else(|| {
                FileError::with_path(
                    &path,
                    LoadError::PathError(std::io::Error::new(
                        ErrorKind::Other,
                        format!("No parent directory for fuchsia_env file {path}"),
                    )),
                )
            })?
            .to_owned();

        let build_dir = resolve_path(&root, contents.fuchsia.config.build_out_dir.as_ref());
        let build_config_file =
            resolve_path(&root, contents.fuchsia.config.build_config_path.as_ref());
        let sdk_root = contents.fuchsia.sdk.path.as_deref().map(|sdk_root| root.join(sdk_root));

        Ok(Self { path, contents, root, build_dir, build_config_file, sdk_root })
    }

    /// Gets the root directory this config domain is part of
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    pub fn get_build_dir(&self) -> Option<&Utf8Path> {
        self.build_dir.as_deref()
    }

    pub fn get_build_config_file(&self) -> Option<&Utf8Path> {
        self.build_config_file.as_deref()
    }

    pub fn get_explicit_sdk_root(&self) -> Option<&Utf8Path> {
        self.sdk_root.as_deref()
    }
}

/// Resolves the given optional ConfigPath in relation to root, potentially
/// going through an indirect file reference to do so. See [`ConfigPath`] for
/// more details on the mechanism.
fn resolve_path(root: &Utf8Path, path: Option<&ConfigPath>) -> Option<Utf8PathBuf> {
    path.and_then(|path| path.resolve(root))
}

/// Checks for adjacent files that could also have been loaded and returns an
/// error if any are found. Returns the extension of the file being loaded if
/// it's the only one.
///
/// # Panics
///
/// It's up to the caller to ensure that the path passed to this has an
/// extension, it will panic if it doesn't.
fn check_adjacent_files<'a>(path: &'a Utf8Path) -> Result<&'a str, FileError> {
    let loading_extension = path.extension().expect("extension on fuchsia_env filename");
    for ext in FILE_EXTENSIONS {
        if *ext != loading_extension && path.with_extension(ext).exists() {
            let path = path.to_owned();
            let kind = LoadError::MultipleFiles(path.with_extension(ext));
            return Err(FileError { path, kind });
        }
    }
    Ok(loading_extension)
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    const TEST_DATA_PATH: &str = env!("TEST_DATA_PATH");

    fn test_data_path() -> &'static Utf8Path {
        Utf8Path::new(TEST_DATA_PATH)
    }

    #[test]
    fn adjacent_files() {
        assert_matches!(
            check_adjacent_files(&test_data_path().join("conflicting_files/fuchsia_env.json5")),
            Err(FileError { kind: LoadError::MultipleFiles(_), .. })
        );
        assert_matches!(
            check_adjacent_files(&test_data_path().join("conflicting_files/fuchsia_env.toml")),
            Err(FileError { kind: LoadError::MultipleFiles(_), .. })
        );
        assert!(
            check_adjacent_files(&test_data_path().join("basic_example/fuchsia_env.toml")).is_ok()
        );
    }

    #[test]
    fn basic_example() {
        let basic_root = test_data_path().join("basic_example").canonicalize_utf8().unwrap();
        let basic_root_env = basic_root.join("fuchsia_env.toml");

        assert_eq!(ConfigDomain::find_root(&basic_root).as_ref(), Some(&basic_root_env));
        assert_eq!(
            ConfigDomain::find_root(&basic_root.join("stuff")).as_ref(),
            Some(&basic_root_env)
        );

        let domain = ConfigDomain::load_from(&basic_root_env).unwrap();
        assert_eq!(
            domain.get_build_config_file(),
            Some(basic_root.join(".fuchsia-build-config.json")).as_deref()
        );
        assert_eq!(domain.get_build_dir(), Some(basic_root.join("bazel-out")).as_deref());
        assert_eq!(
            domain.get_explicit_sdk_root(),
            Some(basic_root.join("bazel-project/external/fuchsia_sdk")).as_deref()
        );
    }
}
