// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fs::File, io::Read};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

pub const FILE_STEM: &str = "fuchsia_env";
pub const FILE_EXTENSIONS: &[&str] = &["toml", "json5"];

/// Represents a path indirection through a file containing the location to
/// actually look.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRef {
    /// The file pointed to by this path should, if present, contain
    /// the actual path that should be loaded. This is used for things like
    /// the `.fx-build-dir` file at the root of the fuchsia.git tree.
    path_ref: Utf8PathBuf,
}

/// A path that will either be just a simple path string that points to a file
/// or an indirect reference to the 'real' location of the file (see [`PathRef`]
/// for more information).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ConfigPath {
    /// A path relative to the configuration domain root
    Relative(Utf8PathBuf),
    /// An indirect reference to a specific path through a file that contains
    /// the 'real' path, when managed by a build system or outside tooling.
    Indirect(PathRef),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuchsiaConfigBootstrap {
    /// A command to run if the file at [`FuchsiaConfig::build_config_path`] isn't
    /// present to try to bootstrap it. It will be run from the configuration
    /// domain root directory, and each element of the array will be passed as
    /// an argument.
    ///
    /// This will only be run if a location for a build config file can be
    /// determined. This means, for example, that if
    /// [`FuchsiaConfig::build_out_dir`] is
    /// set to an indirect ref path, and the redirection file does not exist or
    /// is empty, this will not try to bootstrap it. That means that in an
    /// environment where there can be multiple output directories (such as
    /// in the fuchsia.git tree managed by fx), a separate bootstrapping
    /// command may need to be run to ensure the existence of that file.
    ///
    /// It will be treated as a fatal error if this command exits non-zero and
    /// the file doesn't exist after it exits. If the command exits non-zero,
    /// output from the command will be shown and the tool evaluating it will
    /// exit with the same code.
    command: Option<Vec<String>>,

    /// A command to run to check that the sdk version is up to date before
    /// searching it. This will be run if there is a `fuchsia_lockfile.json`
    /// file at the root of the configuration domain that contains a version for
    /// the SDK, and it doesn't match the version in the discovered SDK.
    ///
    /// If after running the command, the SDK version still doesn't match,
    /// the tool will at the very least print a warning to the user, but may
    /// also error and exit.
    check_sdk: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuchsiaConfig {
    /// Specify that this domain should ignore configuration and defaults
    /// from the global configuration domain, if any.
    #[serde(default)]
    isolated: bool,

    /// In order to support build-level configuration (which will usually
    /// be tool-managed), tools will need to be able to find where those tools
    /// put that file.
    /// If a build output directory is specified, it will be available as a
    /// configuration substitution as `$BUILD_DIR`, and the build config file
    /// path will default to `$BUILD_DIR.json`.
    /// These paths are always relative to the project root, unless this is
    /// a global file, in which case they are either relative to the homedir
    /// or must be absolute.
    /// An active build output directory can optionally be specified directly,
    /// or as a `path_ref` as with the sdk. This would be used in-tree and
    /// point to `.fx-build-dir`. See [`ConfigPath`] for more information.
    build_out_dir: Option<ConfigPath>,
    /// And/or a location for the build configuration file can be specified
    /// in the same ways. As a direct file path or an indirect reference.
    build_config_path: Option<ConfigPath>,

    /// Settings that affect bootstrapping and keeping the sdk up to date, if
    /// it's vendored.
    #[serde(default)]
    bootstrap: FuchsiaConfigBootstrap,

    /// Any key specified here will be treated as a default configuration
    /// key and value for this configuration domain. This has a pretty
    /// similar effect to the current behavior of the “global” config
    /// level for projects that use it.
    defaults: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuchsiaSdk {
    /// If the SDK is vendored into this project you can specify a path to it.
    /// Otherwise, normal configuration logic will be used to find it based on
    /// the “sdk.root” key.
    path: Option<Utf8PathBuf>,
}

/// Keys meant for any tool to consume are in the [fuchsia] section.
/// These keys should not change often, because they must be
/// interpretable by the broadest range of versions. The `fuchsia`
/// top level key also *must* be present for this file to be
/// interpreted as a config domain control file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Fuchsia {
    #[serde(default)]
    sdk: FuchsiaSdk,
    #[serde(default)]
    config: FuchsiaConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuchsiaEnv {
    fuchsia: Fuchsia,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ParseError {
    #[error("Don't know how to parse a `{0}` file")]
    UnknownFormat(String),
    #[error("Error reading buffer")]
    Io(#[from] std::io::Error),
    #[error("JSON5 Parse Error")]
    Json5(#[from] serde_json5::Error),
    #[error("TOML Parse Error")]
    Toml(#[from] toml::de::Error),
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum LoadError {
    #[error("Multiple candidate files were present (also found `{0}`), remove one of them to resolve the ambiguity")]
    MultipleFiles(Utf8PathBuf),
    #[error(transparent)]
    Parsing(#[from] ParseError),
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

impl FuchsiaEnv {
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

    pub fn load_toml(mut file: impl Read) -> Result<Self, ParseError> {
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        Ok(toml::from_slice(&bytes).map_err(ParseError::from)?)
    }

    pub fn load_json5(mut file: impl Read) -> Result<Self, ParseError> {
        serde_json5::from_reader(&mut file).map_err(ParseError::from)
    }

    /// Loads the fuchsia_env file with the format indicated by its extension.
    pub fn load_from(path: &Utf8Path) -> Result<Self, FileError> {
        let loading_extension = check_adjacent_files(path)?;

        let file = File::open(path).map_err(|e| FileError::with_path(path, e))?;

        match loading_extension {
            "toml" => Self::load_toml(file).map_err(|e| FileError::with_path(path, e)),
            "json5" => Self::load_json5(file).map_err(|e| FileError::with_path(path, e)),
            _ => Err(FileError::with_path(
                path,
                ParseError::UnknownFormat(loading_extension.to_owned()),
            )),
        }
    }
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
    use serde_json::{json, Value};

    pub use super::*;

    #[test]
    fn config_paths() {
        fn parse(json: Value) -> serde_json::Result<ConfigPath> {
            serde_json::from_value(json)
        }

        assert_eq!(
            parse(json!("tmp/blah")).unwrap(),
            ConfigPath::Relative("tmp/blah".into()),
            "relative path as simple string"
        );
        assert_eq!(
            parse(json!({ "path_ref": "tmp/blah" })).unwrap(),
            ConfigPath::Indirect(PathRef { path_ref: "tmp/blah".into() }),
            "path reference as object"
        );
        parse(json!({ "something else": "is wrong" }))
            .expect_err("a different kind of object should fail");
    }

    #[test]
    fn invalid_toml_files_fail() {
        FuchsiaEnv::load_toml(&b""[..]).expect_err("failure parsing empty file");
        FuchsiaEnv::load_toml(&b"[some-other-key]"[..]).expect_err("failure parsing empty file");
    }
    #[test]
    fn invalid_json5_files_fail() {
        FuchsiaEnv::load_json5(&b""[..]).expect_err("failure parsing empty file");
        FuchsiaEnv::load_json5(&b"{}"[..]).expect_err("failure parsing empty file");
        FuchsiaEnv::load_json5(&br#"{"some-key": "some-value"}"#[..])
            .expect_err("failure parsing empty file");
    }

    #[test]
    fn simple_toml_file_ok() {
        FuchsiaEnv::load_toml(&b"[fuchsia]"[..]).expect("successful parse");
    }
    #[test]
    fn simple_json5_file_ok() {
        FuchsiaEnv::load_json5(&br#"{"fuchsia": {}}"#[..]).expect("successful parse");
    }
}
