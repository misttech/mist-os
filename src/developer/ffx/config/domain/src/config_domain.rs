// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::file_states::FileStates;
use crate::fuchsia_env::{ConfigMap, ConfigPath, FuchsiaEnv, ParseError};

const FILE_STEM: &str = "fuchsia_env";
const TOML_FILE_EXTENSION: &str = "toml";
const JSON5_FILE_EXTENSION: &str = "json5";
const FILE_EXTENSIONS: &[&str] = &[TOML_FILE_EXTENSION, JSON5_FILE_EXTENSION];

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
enum LoadError {
    #[error("Multiple candidate files were present (also found `{0}`), remove one of them to resolve the ambiguity")]
    MultipleFiles(Utf8PathBuf),
    #[error(transparent)]
    Parsing(#[from] ParseError),
    #[error("Couldn't determine canonical project root path")]
    PathError(std::io::Error),
    #[error("No command name in command line entry")]
    InvalidCommand,
    #[error("Error reading file")]
    Io(#[from] std::io::Error),
    #[error("Path is not absolute")]
    NotAbsolute,
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

#[derive(Debug, Clone, PartialEq)]
struct ConfigCommand {
    cmd: String,
    args: Vec<String>,
}

impl From<&ConfigCommand> for Command {
    fn from(value: &ConfigCommand) -> Self {
        let mut cmd = Command::new(&value.cmd);
        cmd.args(value.args.iter());
        cmd
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDomain {
    root: Utf8PathBuf,
    path: Utf8PathBuf,
    build_dir: Option<Utf8PathBuf>,
    build_config_file: Option<Utf8PathBuf>,
    bootstrap_cmd: Option<ConfigCommand>,
    sdk_check_cmd: Option<ConfigCommand>,
    contents: FuchsiaEnv,
}

impl ConfigDomain {
    /// The relative path from project root that fuchsia tools can store
    /// project-specific information like lock files.
    const FUCHSIA_PROJECT_DATA: &'static str = ".fuchsia";
    /// The relative path from [`ConfigDomain::FUCHSIA_PROJECT_DATA`] for the file that
    /// stores hashes of files referenced by a project's
    /// `fuchsia.sdk.version_check_command` field.
    const VERSION_CHECK_MANIFEST: &'static str = "sdk-version-check.manifest";

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

        Self::load_from_contents(path, contents)
    }

    /// Loads the [`FuchsiaEnv`] object as if it were at the given path. Mostly
    /// useful for testing, but may also be useful for creating 'virtual' config
    /// domains.
    pub fn load_from_contents(path: Utf8PathBuf, contents: FuchsiaEnv) -> Result<Self, FileError> {
        if !path.is_absolute() {
            return Err(FileError::with_path(&path, LoadError::NotAbsolute));
        }

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

        let build_dir = resolve_path(&root, contents.fuchsia.project.build_out_dir.as_ref(), None);
        let build_config_file = resolve_path(
            &root,
            contents.fuchsia.project.build_config_path.as_ref(),
            build_dir.as_deref(),
        );
        let bootstrap_cmd = resolve_command(contents.fuchsia.project.bootstrap_command.as_deref())
            .map_err(|e| FileError::with_path(&path, e))?;
        let sdk_check_cmd = resolve_command(contents.fuchsia.sdk.version_check_command.as_deref())
            .map_err(|e| FileError::with_path(&path, e))?;

        Ok(Self {
            path,
            contents,
            root,
            build_dir,
            build_config_file,
            bootstrap_cmd,
            sdk_check_cmd,
        })
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

    pub fn get_config_defaults(&self) -> &ConfigMap {
        &self.contents.fuchsia.project.default_config
    }

    /// If the files necessary to find project-specific configuration aren't
    /// available, return the configured command to make them exist.
    pub fn needs_config_bootstrap(&mut self) -> Option<Command> {
        // don't even bother if we don't have a bootstrap command configured
        let bootstrap_cmd = basic_command_args(self, self.bootstrap_cmd.as_ref()?);
        let build_dir_config = self.contents.fuchsia.project.build_out_dir.as_ref();
        let build_config_file_config = self.contents.fuchsia.project.build_config_path.as_ref();

        // update the build dir and config paths if necessary
        if self.build_dir.is_none() {
            self.build_dir = resolve_path(&self.root, build_dir_config, None);
        }
        if self.build_config_file.is_none() {
            self.build_config_file =
                resolve_path(&self.root, build_config_file_config, self.build_dir.as_deref());
        }

        // see if the files exist or not.
        let wants_bootstrap =
            config_path_needs_bootstrap(build_dir_config, self.build_dir.as_deref())
                || config_path_needs_bootstrap(
                    build_config_file_config,
                    self.build_config_file.as_deref(),
                );

        wants_bootstrap.then_some(bootstrap_cmd)
    }

    /// If either of the following are true then return the configured command
    /// to update the sdk:
    /// - the files that pin the version of the SDK for this project have
    /// changed since last checked (based on `known_states`).
    /// - the SDK does not exist at `sdk_root`.
    ///
    /// `known_states` will be updated with the new states if they've changed.
    pub fn needs_sdk_update(
        &self,
        sdk_root: &sdk::SdkRoot,
        known_states: &mut FileStates,
    ) -> Option<Command> {
        // don't even bother if we don't have an update command configured.
        let mut update_cmd = basic_command_args(self, self.sdk_check_cmd.as_ref()?);
        let files = self.contents.fuchsia.sdk.version_check_files.as_deref()?;
        update_cmd.env("FUCHSIA_VERSION_CHECK_FILES", join_paths(files));

        // check that the SDK exists at the given path.
        if !sdk_root.manifest_exists() {
            tracing::trace!("SDK manifest didn't exist for {sdk_root:?}");
            return Some(update_cmd);
        } else {
            tracing::trace!("SDK manifest existed for {sdk_root:?}");
        }

        tracing::trace!("checking files: {files:?}");
        match FileStates::check_paths(self.root.to_owned(), files) {
            Ok(new_states) => {
                tracing::trace!("comparing file state: {known_states:?} == {new_states:?}");
                if let Err(changed) = known_states.changed_files(&new_states) {
                    update_cmd.env("FUCHSIA_VERSION_CHECK_FILES_DIRTY", join_paths(changed.iter()));
                    *known_states = new_states;
                    Some(update_cmd)
                } else {
                    None
                }
            }
            Err(err) => {
                tracing::warn!("Error reading SDK state file: {err}");
                Some(update_cmd)
            }
        }
    }

    /// The path relative to the project root where the version check manifest
    /// file is stored
    fn sdk_check_manifest_path(&self) -> Utf8PathBuf {
        self.root.join(Self::FUCHSIA_PROJECT_DATA).join(Self::VERSION_CHECK_MANIFEST)
    }

    /// Loads the sdk check manifest for this config domain, if any are configured.
    /// If the file doesn't exist, or doesn't parse, it will return a "default"
    /// [`FileStates`] object with nothing in it. If no `version_check_files`
    /// are configured, it will return [`None`].
    pub fn load_sdk_check_manifest(&self) -> Option<FileStates> {
        if self.contents.fuchsia.sdk.version_check_files.is_some() {
            // try and load the manifest, ignoring errors and treating it as
            // invalid.
            let manifest_path = self.sdk_check_manifest_path();
            let manifest = File::open(manifest_path)
                .ok()
                .and_then(|f| serde_json::from_reader(f).ok())
                .unwrap_or_else(FileStates::default);
            Some(manifest)
        } else {
            None
        }
    }

    /// Saves the sdk check manifest for this config domain
    pub fn save_sdk_check_manifest(&self, manifest: &FileStates) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.root.join(Self::FUCHSIA_PROJECT_DATA))?;
        let manifest_path = self.sdk_check_manifest_path();
        let file = File::create(manifest_path)?;
        Ok(serde_json::to_writer(file, manifest)?)
    }
}

fn join_paths(paths: impl IntoIterator<Item = impl Display>) -> String {
    itertools::join(paths, ";")
}

fn basic_command_args(domain: &ConfigDomain, cmd: &ConfigCommand) -> Command {
    let mut cmd = Command::from(cmd);
    cmd.current_dir(&domain.root);
    cmd.env("FUCHSIA_ENV_PATH", &domain.path);
    if let Some(out_dir) = &domain.build_dir {
        cmd.env("FUCHSIA_OUT_DIR", &out_dir);
    }
    if let Some(config_path) = &domain.build_config_file {
        cmd.env("FUCHSIA_BUILD_CONFIG_PATH", &config_path);
    }
    cmd
}

fn resolve_command(cmd_slice: Option<&[String]>) -> Result<Option<ConfigCommand>, LoadError> {
    let Some(cmd_slice) = cmd_slice else {
        return Ok(None);
    };
    let mut cmd_iter = cmd_slice.iter().cloned();
    let Some(cmd) = cmd_iter.next() else {
        return Err(LoadError::InvalidCommand);
    };
    let args = Vec::from_iter(cmd_iter);

    Ok(Some(ConfigCommand { cmd, args }))
}

fn config_path_needs_bootstrap(config: Option<&ConfigPath>, path: Option<&Utf8Path>) -> bool {
    match (config, path) {
        // needs bootstrap if it's set and the directory doesn't exist or isn't valid
        (Some(_), Some(path)) if !path.exists() => true,
        // needs bootstrap if it's set and the configpath didn't resolve
        (Some(_), None) => true,
        // otherwise doesn't need bootstrap
        _ => false,
    }
}

fn resolve_path_ref(path_ref: &Utf8Path, root: &Utf8Path) -> Option<Utf8PathBuf> {
    let path_ref_file = root.join(path_ref);
    let contents = BufReader::new(File::open(&path_ref_file).ok()?);
    let inner_path = Utf8PathBuf::from(&contents.lines().next()?.ok()?);
    Some(root.join(&inner_path))
}

/// Resolves the given optional ConfigPath in relation to root, potentially
/// going through an indirect file reference to do so. See [`ConfigPath`] for
/// more details on the mechanism.
fn resolve_path(
    root: &Utf8Path,
    path: Option<&ConfigPath>,
    build_dir: Option<&Utf8Path>,
) -> Option<Utf8PathBuf> {
    match path? {
        ConfigPath::Relative(path) => Some(root.join(path)),
        ConfigPath::PathRef { path_ref } => resolve_path_ref(path_ref, root),
        ConfigPath::OutDirRef { out_dir_ref } => {
            build_dir.map(|build_dir| build_dir.join(out_dir_ref))
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
    use assert_matches::assert_matches;
    use sdk::SdkRoot;

    use super::*;
    use crate::tests::*;

    #[fuchsia::test]
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

    #[fuchsia::test]
    fn basic_example() {
        let basic_root = test_data_path().join("basic_example").canonicalize_utf8().unwrap();
        let basic_root_env = basic_root.join("fuchsia_env.toml");

        assert_eq!(ConfigDomain::find_root(&basic_root).as_ref(), Some(&basic_root_env));
        assert_eq!(
            ConfigDomain::find_root(&basic_root.join("stuff")).as_ref(),
            Some(&basic_root_env)
        );

        let mut domain = ConfigDomain::load_from(&basic_root_env).unwrap();
        assert_eq!(
            domain.get_build_config_file(),
            Some(basic_root.join(".fuchsia-build-config.json")).as_deref()
        );
        assert_eq!(domain.get_build_dir(), Some(basic_root.join("bazel-out")).as_deref());

        assert_matches!(
            domain.needs_config_bootstrap(),
            Some(_),
            "basic example lacks a config file, so would need bootstrap"
        );
    }

    #[fuchsia::test]
    fn rfc_example() {
        let rfc_root = test_data_path().join("rfc_example").canonicalize_utf8().unwrap();
        let rfc_root_env = rfc_root.join("fuchsia_env.toml");

        assert_eq!(ConfigDomain::find_root(&rfc_root).as_ref(), Some(&rfc_root_env));
        assert_eq!(ConfigDomain::find_root(&rfc_root.join("stuff")).as_ref(), Some(&rfc_root_env));

        let mut domain = ConfigDomain::load_from(&rfc_root_env).unwrap();
        assert_eq!(domain.get_build_dir(), Some(rfc_root.join("out")).as_deref());
        assert_eq!(
            domain.get_build_config_file(),
            Some(rfc_root.join("out/fuchsia_build_config.json")).as_deref()
        );
        assert_matches!(
            domain.needs_config_bootstrap(),
            None,
            "sdk example has a config file, so would not need bootstrap"
        );

        let rfc_root_sdk = domain.get_build_dir().unwrap().to_owned();
        let rfc_root_states =
            FileStates::check_paths(rfc_root.clone(), &["manifest/bazel_sdk.ensure"]).unwrap();
        let mut known_state = FileStates::default();
        assert_matches!(
            domain.needs_sdk_update(&SdkRoot::Full(rfc_root_sdk.clone().into()), &mut known_state),
            Some(_),
            "sdk example with different existing state should need updating"
        );
        assert_eq!(
            rfc_root_states, known_state,
            "state should be updated with the new state after check"
        );
        assert_matches!(
            domain.needs_sdk_update(
                &SdkRoot::Full(rfc_root_sdk.into()),
                &mut rfc_root_states.clone()
            ),
            None,
            "sdk example with correct state should not need updating"
        );
        assert_matches!(
            domain.needs_sdk_update(&SdkRoot::Full(rfc_root.into()), &mut rfc_root_states.clone()),
            Some(_),
            "sdk example with invalid sdk root should need updating"
        );
    }

    #[fuchsia::test]
    fn build_dir_ref_path() {
        let basic_root = test_data_path().join("build_dir_path_ref").canonicalize_utf8().unwrap();
        let basic_root_env = basic_root.join("fuchsia_env.toml");

        let domain = ConfigDomain::load_from(&basic_root_env).unwrap();
        assert_eq!(domain.get_build_dir(), Some(basic_root.join("build-dir")).as_deref(),);
    }

    #[fuchsia::test]
    fn basic_config_path_resolution() {
        let path_ref_root = test_data_path().join("path_refs");

        assert_eq!(
            resolve_path("/tmp/blah".into(), Some(&ConfigPath::Relative("hi".into())), None),
            Some("/tmp/blah/hi".into())
        );
        assert_eq!(
            resolve_path(
                &path_ref_root,
                Some(&ConfigPath::PathRef { path_ref: "does-not-exist".into() }),
                None
            ),
            None
        );
        assert_eq!(
            resolve_path(
                &path_ref_root,
                Some(&ConfigPath::PathRef { path_ref: "empty-path-ref".into() }),
                None
            ),
            None
        );
        assert_eq!(
            resolve_path(
                &path_ref_root,
                Some(&ConfigPath::PathRef { path_ref: "path-ref-to-absolute".into() }),
                None
            ),
            Some("/tmp/blah".into())
        );
        assert_eq!(
            resolve_path(
                &path_ref_root,
                Some(&ConfigPath::PathRef { path_ref: "path-ref-to-relative".into() }),
                None
            ),
            Some(path_ref_root.join("build-config-file"))
        );
    }
}
