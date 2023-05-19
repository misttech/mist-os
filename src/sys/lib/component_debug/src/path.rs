// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::io::{Directory, DirentKind, RemoteDirectory},
    anyhow::{anyhow, bail, Error, Result},
    fidl_fuchsia_io as fio,
    std::path::{Component, PathBuf},
};

/// Returns a valid file path to write to given a source and destination path.
///
/// The purpose of this function is to help infer a path in cases which an ending file name for the destination path is not provided.
/// For example, the command "ffx component storage copy ~/alarm.wav [instance-id]::/" does not know what name to give the new file copied.
/// [instance-id]::/. Thus it is necessary to infer this new file name and generate the new path "[instance-id]::/alarm.wav".
///
/// This function will check the current destination path and return a new path if the final component in the path is a directory.
/// The new path will have the filename of the source path appended to the destination path.
/// Otherwise, the destination path passed in will be returned.
///
/// # Arguments
///
/// * `destination_dir`: RemoteDirectory proxy to help retrieve directory entries on device
/// * `source_path`:  source path from which to
/// * `path`: source path of either a host filepath or a component namespace entry
///
/// # Error Conditions:
///
/// * File name for `source_path` is empty
/// * Communications error talking to remote endpoint
pub async fn add_source_filename_to_path_if_absent(
    destination_dir: &RemoteDirectory,
    source_path: HostOrRemotePath,
    path: HostOrRemotePath,
) -> Result<PathBuf, Error> {
    let source_file = match source_path {
        HostOrRemotePath::Host(path) => path
            .file_name()
            .map_or_else(|| Err(anyhow!("Source path is empty")), |file| Ok(PathBuf::from(file)))?,
        HostOrRemotePath::Remote(path) => path
            .relative_path
            .file_name()
            .map_or_else(|| Err(anyhow!("Source path is empty")), |file| Ok(PathBuf::from(file)))?,
    };

    let source_file_str = source_file.display().to_string();
    // If the destination is a directory, append `source_file_str`.
    match path {
        HostOrRemotePath::Host(mut path) => {
            let destination_file = path.file_name();

            if destination_file.is_none() || path.is_dir() {
                path.push(&source_file_str);
            }

            Ok(path)
        }
        HostOrRemotePath::Remote(remote_path) => {
            let mut path = remote_path.relative_path;
            let destination_file = path.file_name();
            let remote_destination_type = if destination_file.is_some() {
                destination_dir
                    .entry_type(destination_file.unwrap_or_default().to_str().unwrap_or_default())
                    .await?
            } else {
                destination_dir.entry_type(&source_file_str).await?
            };

            match (&remote_destination_type, &destination_file) {
                (Some(kind), _) => {
                    match kind {
                        DirentKind::File => {}
                        // TODO(https://fxbug.dev/127335): Update component_manager vfs to assign proper DirentKinds when installing the directory tree.
                        DirentKind::Directory => {
                            path.push(&source_file_str);
                        }
                    }
                    Ok(path)
                }
                (None, Some(_)) => Ok(path),
                (None, None) => {
                    path.push(&source_file_str);
                    Ok(path)
                }
            }
        }
    }
}

pub const REMOTE_PATH_HELP: &'static str = r#"Remote paths have the following formats:
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
                        c => bail!("Unsupported path object: {:?}. {}", c, REMOTE_PATH_HELP),
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
pub enum HostOrRemotePath {
    Host(PathBuf),
    Remote(RemotePath),
}

impl HostOrRemotePath {
    pub fn parse(path: &str) -> HostOrRemotePath {
        match RemotePath::parse(path) {
            Ok(path) => HostOrRemotePath::Remote(path),
            // If we can't parse a remote path, then it is a host path.
            Err(_) => HostOrRemotePath::Host(PathBuf::from(path)),
        }
    }
}

pub struct NamespacedPath {
    pub path: RemotePath,
    pub ns: fio::DirectoryProxy,
}
