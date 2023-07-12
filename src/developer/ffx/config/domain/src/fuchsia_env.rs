// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

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

impl<T: AsRef<str>> From<T> for PathRef {
    fn from(value: T) -> Self {
        let path_ref = value.as_ref().into();
        Self { path_ref }
    }
}

impl PathRef {
    pub fn resolve(&self, root: &Utf8Path) -> Option<Utf8PathBuf> {
        let path_ref_file = root.join(&self.path_ref);
        let contents = BufReader::new(File::open(&path_ref_file).ok()?);
        let inner_path = Utf8PathBuf::from(&contents.lines().next()?.ok()?);
        Some(root.join(&inner_path))
    }
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

impl ConfigPath {
    pub fn resolve(&self, root: &Utf8Path) -> Option<Utf8PathBuf> {
        match self {
            ConfigPath::Relative(path) => Some(root.join(path)),
            ConfigPath::Indirect(path_ref) => path_ref.resolve(root),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// It will be treated as a fatal error if this command exits successfully
    /// (with a zero exit code) and the file doesn't exist after it exits. If
    /// the command exits non-zero, output from the command will be shown and
    /// the tool evaluating it will exit with the same code.
    pub command: Option<Vec<String>>,

    /// A command to run to check that the sdk version is up to date before
    /// searching it. This will be run if there is a `fuchsia_lockfile.json`
    /// file at the root of the configuration domain that contains a version for
    /// the SDK, and it doesn't match the version in the discovered SDK.
    ///
    /// If after running the command, the SDK version still doesn't match,
    /// the tool will at the very least print a warning to the user, but may
    /// also error and exit.
    pub check_sdk: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuchsiaConfig {
    /// Specify that this domain should ignore configuration and defaults
    /// from the global configuration domain, if any.
    #[serde(default)]
    pub isolated: bool,

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
    pub build_out_dir: Option<ConfigPath>,
    /// And/or a location for the build configuration file can be specified
    /// in the same ways. As a direct file path or an indirect reference.
    pub build_config_path: Option<ConfigPath>,

    /// Settings that affect bootstrapping and keeping the sdk up to date, if
    /// it's vendored.
    #[serde(default)]
    pub bootstrap: FuchsiaConfigBootstrap,

    /// Any key specified here will be treated as a default configuration
    /// key and value for this configuration domain. This has a pretty
    /// similar effect to the current behavior of the “global” config
    /// level for projects that use it.
    #[serde(default)]
    pub defaults: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuchsiaSdk {
    /// If the SDK is vendored into this project you can specify a path to it.
    /// Otherwise, normal configuration logic will be used to find it based on
    /// the “sdk.root” key.
    pub path: Option<Utf8PathBuf>,
}

/// Keys meant for any tool to consume are in the [fuchsia] section.
/// These keys should not change often, because they must be
/// interpretable by the broadest range of versions. The `fuchsia`
/// top level key also *must* be present for this file to be
/// interpreted as a config domain control file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fuchsia {
    #[serde(default)]
    pub sdk: FuchsiaSdk,
    #[serde(default)]
    pub config: FuchsiaConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuchsiaEnv {
    pub fuchsia: Fuchsia,
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

impl FuchsiaEnv {
    pub fn load_toml(mut file: impl Read) -> Result<Self, ParseError> {
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        Ok(toml::from_slice(&bytes).map_err(ParseError::from)?)
    }

    pub fn load_json5(mut file: impl Read) -> Result<Self, ParseError> {
        serde_json5::from_reader(&mut file).map_err(ParseError::from)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    pub use super::*;
    pub use crate::tests::*;

    #[test]
    fn parsing_config_paths() {
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
    fn config_path_resolution() {
        let path_ref_root = test_data_path().join("path_refs");

        assert_eq!(
            ConfigPath::Relative("hi".into()).resolve("/tmp/blah".into()),
            Some("/tmp/blah/hi".into())
        );
        assert_eq!(ConfigPath::Indirect("does-not-exist".into()).resolve(&path_ref_root), None);
        assert_eq!(ConfigPath::Indirect("empty-path-ref".into()).resolve(&path_ref_root), None);
        assert_eq!(
            ConfigPath::Indirect("path-ref-to-absolute".into()).resolve(&path_ref_root),
            Some("/tmp/blah".into())
        );
        assert_eq!(
            ConfigPath::Indirect("path-ref-to-relative".into()).resolve(&path_ref_root),
            Some(path_ref_root.join("build-config-file"))
        );
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
    fn empty_sections_toml_file_ok() {
        let examples: [&'static [u8]; 4] =
            [b"[fuchsia]", b"[fuchsia.config]", b"[fuchsia.config.bootstrap]", b"[fuchsia.sdk]"];

        for example in examples {
            assert_eq!(FuchsiaEnv::load_toml(example).unwrap(), Default::default());
        }
    }
    #[test]
    fn empty_sections_json5_file_ok() {
        let examples: [&'static [u8]; 4] = [
            br#"{"fuchsia": {}}"#,
            br#"{"fuchsia": {"config": {}}}"#,
            br#"{"fuchsia": {"config": {"bootstrap": {}}}}"#,
            br#"{"fuchsia": {"sdk": {}}}"#,
        ];

        for example in examples {
            assert_eq!(FuchsiaEnv::load_json5(example).unwrap(), Default::default());
        }
    }

    #[test]
    fn config_set_toml_file_ok() {}
    #[test]
    fn config_set_json5_file_ok() {}
}
