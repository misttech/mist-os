// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::io::{Directory, DirentKind},
    anyhow::{anyhow, bail, Error, Result},
    std::path::{Component, PathBuf},
};

pub const REMOTE_PATH_HELP: &'static str = r#"Remote paths allow the following formats:
1)  [instance ID]::[path relative to storage]
    Example: "c1a6d0aebbf7c092c53e8e696636af8ec0629ff39b7f2e548430b0034d809da4::/path/to/file"

    `..` is not valid anywhere in the remote path.

    To learn about component instance IDs, see https://fuchsia.dev/go/components/instance-id

2) [absolute moniker]::[path in namespace]
    Example: /foo/bar::/config/data/sample.json

   To learn more about absolute monikers, see https://fuchsia.dev/go/components/moniker#absolute"#;

#[derive(Clone)]
pub struct RemotePath {
    pub remote_id: String,
    pub relative_path: PathBuf,
}

/// Represents a path to a component instance.
/// Refer to REMOTE_PATH_HELP above for the format of RemotePath.
impl RemotePath {
    pub fn parse(input: &str) -> Result<Self> {
        match input.split_once("::") {
            Some((first, second)) => {
                if second.contains("::") {
                    bail!(
                        "Remote path must contain exactly one `::` separator. {}",
                        REMOTE_PATH_HELP
                    )
                }

                let remote_id = first.to_string();
                let relative_path = PathBuf::from(second);

                // Perform checks on path that ignore `.`  and disallow `..`, `/` or Windows path prefixes such as C: or \\
                let mut normalized_relative_path = PathBuf::new();
                for component in relative_path.components() {
                    match component {
                        Component::Normal(c) => normalized_relative_path.push(c),
                        Component::RootDir => continue,
                        Component::CurDir => continue,
                        c => bail!("Unsupported path component: {:?}. {}", c, REMOTE_PATH_HELP),
                    }
                }

                Ok(Self { remote_id, relative_path: normalized_relative_path })
            }
            None => {
                bail!("Remote path must contain exactly one `::` separator. {}", REMOTE_PATH_HELP)
            }
        }
    }

    pub fn contains_wildcard(&self) -> bool {
        return self.to_string().contains("*");
    }

    pub fn relative_path_string(&self) -> String {
        return self.relative_path.to_string_lossy().to_string();
    }
}

impl ToString for RemotePath {
    fn to_string(&self) -> String {
        format!("{}::/{}", self.remote_id, self.relative_path.to_string_lossy().to_string())
    }
}

#[derive(Clone)]
/// Represents either a local path to a file or directory, or the path to a file/directory
/// within a component.
pub enum LocalOrRemotePath {
    Local(PathBuf),
    Remote(RemotePath),
}

impl LocalOrRemotePath {
    pub fn parse(path: &str) -> LocalOrRemotePath {
        match RemotePath::parse(path) {
            Ok(path) => LocalOrRemotePath::Remote(path),
            // If we can't parse a remote path, then it is a host path.
            Err(_) => LocalOrRemotePath::Local(PathBuf::from(path)),
        }
    }
}

/// Returns a readable `Directory` by opening the parent dir of `path`.
///
/// * `path`: The path from which to derive the parent
/// * `dir`: RemoteDirectory to on which to open a subdir.
pub fn open_parent_subdir_readable<D: Directory>(path: &PathBuf, dir: &D) -> Result<D> {
    if path.components().count() < 2 {
        // The path is something like "/foo" which, as a relative path from `dir`, would make `dir`
        // the parent.
        return dir.clone();
    }

    dir.open_dir_readonly(path.parent().unwrap())
}

/// If `destination_path` in `destination_dir` is itself a directory, returns
/// a path with the filename portion of `source_path` appended. Otherwise, returns
/// a copy of the input `destination_path`.
///
/// The purpose of this function is to help infer a path in cases which an ending file name for the destination path is not provided.
/// For example, the command "ffx component storage copy ~/alarm.wav [instance-id]::/" does not know what name to give the new file copied.
/// [instance-id]::/. Thus it is necessary to infer this new file name and generate the new path "[instance-id]::/alarm.wav".
///
/// # Arguments
///
/// * `destination_dir`: Directory to query for the type of `destination_path`
/// * `source_path`: path from which to read a filename, if needed
/// * `destination_path`: destination path
///
/// # Error Conditions:
///
/// * File name for `source_path` is empty
/// * Communications error talking to remote endpoint
pub async fn add_source_filename_to_path_if_absent<D: Directory>(
    destination_dir: &D,
    source_path: &PathBuf,
    destination_path: &PathBuf,
) -> Result<PathBuf, Error> {
    let source_file = source_path
        .file_name()
        .map_or_else(|| Err(anyhow!("Source path is empty")), |file| Ok(PathBuf::from(file)))?;
    let source_file_str = source_file.display().to_string();

    // If the destination is a directory, append `source_file_str`.
    if let Some(destination_file) = destination_path.file_name() {
        let parent_dir = open_parent_subdir_readable(destination_path, destination_dir)?;
        match parent_dir.entry_type(&destination_file.to_string_lossy().to_string()).await? {
            Some(DirentKind::File) | None => Ok(destination_path.clone()),
            Some(DirentKind::Directory) => Ok(destination_path.join(source_file_str)),
        }
    } else {
        Ok(destination_path.join(source_file_str))
    }
}
