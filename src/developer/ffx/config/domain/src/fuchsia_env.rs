// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Read;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// A serde_json::Map used for generic config values
pub type ConfigMap = serde_json::Map<String, serde_json::Value>;

/// A path that will either be just a simple path string that points to a file
/// or an indirect reference to the 'real' location of the file (see [`PathRef`]
/// for more information).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ConfigPath {
    /// A path relative to the configuration domain root
    Relative(Utf8PathBuf),
    /// The file pointed to by this path should, if present, contain
    /// the actual path that should be loaded. This is used for things like
    /// the `.fx-build-dir` file at the root of the fuchsia.git tree.
    PathRef { path_ref: Utf8PathBuf },
    /// A path relative to the [`FuchsiaProject::build_out_dir`]
    OutDirRef { out_dir_ref: Utf8PathBuf },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuchsiaProject {
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
    #[serde(alias = "build-out-dir")]
    pub build_out_dir: Option<ConfigPath>,
    /// And/or a location for the build configuration file can be specified
    /// in the same ways. As a direct file path or an indirect reference.
    #[serde(alias = "build-config-path")]
    pub build_config_path: Option<ConfigPath>,

    /// A command to run if the file at [`FuchsiaProject::build_config_path`]
    /// isn't present to try to bootstrap it. It will be run from the
    /// configuration domain root directory, and each element of the array will
    /// be passed as an argument.
    ///
    /// It will be treated as a fatal error if this command exits successfully
    /// (with a zero exit code) and the file doesn't exist after it exits. If
    /// the command exits non-zero, output from the command will be shown and
    /// the tool evaluating it will exit with the same code.
    #[serde(alias = "bootstrap-command")]
    pub bootstrap_command: Option<Vec<String>>,

    /// Any key specified here will be treated as a default configuration
    /// key and value for this configuration domain. This has a pretty
    /// similar effect to the current behavior of the “global” config
    /// level for projects that use it.
    #[serde(default)]
    #[serde(alias = "defaults", alias = "default-config")]
    pub default_config: ConfigMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuchsiaSdk {
    /// The location within the project root of files that the project tooling
    /// use to control the version of the SDK in use.
    #[serde(alias = "version-check-files")]
    pub version_check_files: Option<Vec<Utf8PathBuf>>,
    /// A command to run to check that the sdk version is up to date before
    /// searching it. This will be run if any of the files in
    /// [`FuchsiaSdk::version_check_files`] has changed since they were last
    /// recorded
    #[serde(alias = "version-check-command")]
    pub version_check_command: Option<Vec<String>>,
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
    #[serde(alias = "config")]
    pub project: FuchsiaProject,
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
            ConfigPath::PathRef { path_ref: "tmp/blah".into() },
            "path reference as object"
        );
        assert_eq!(
            parse(json!({ "out_dir_ref": "stuff" })).unwrap(),
            ConfigPath::OutDirRef { out_dir_ref: "stuff".into() },
            "out dir reference as object"
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
    fn empty_sections_toml_file_ok() {
        let examples: [&'static [u8]; 4] =
            [b"[fuchsia]", b"[fuchsia.project]", b"[fuchsia.project.bootstrap]", b"[fuchsia.sdk]"];

        for example in examples {
            assert_eq!(FuchsiaEnv::load_toml(example).unwrap(), Default::default());
        }
    }
    #[test]
    fn empty_sections_json5_file_ok() {
        let examples: [&'static [u8]; 4] = [
            br#"{"fuchsia": {}}"#,
            br#"{"fuchsia": {"project": {}}}"#,
            br#"{"fuchsia": {"project": {"default_config": {}}}}"#,
            br#"{"fuchsia": {"sdk": {}}}"#,
        ];

        for example in examples {
            assert_eq!(FuchsiaEnv::load_json5(example).unwrap(), Default::default());
        }
    }
}
