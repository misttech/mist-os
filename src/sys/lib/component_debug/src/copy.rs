// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        io::Directory,
        path::{
            normalize_destination, HostOrRemotePath, NamespacedPath, RemotePath, REMOTE_PATH_HELP,
        },
    },
    anyhow::{bail, Result},
    fidl::endpoints::{create_endpoints, ClientEnd},
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    fuchsia_fs::directory::DirentKind,
    regex::Regex,
    std::{
        collections::HashMap,
        fs::{read, write},
        path::PathBuf,
    },
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum CopyError {
    #[error("Destination can not have a wildcard.")]
    DestinationContainWildcard,

    #[error("At least two paths (host or remote) must be provided.")]
    NotEnoughPaths,

    #[error("Could not write to host: {error}.")]
    FailedToWriteToHost { error: std::io::Error },

    #[error("File name was unexpectedly empty.")]
    EmptyFileName,

    #[error("Path does not contain a parent folder.")]
    NoParentFolder { path: String },

    #[error("Could not find files in device that matched pattern: {pattern}.")]
    NoWildCardMatches { pattern: String },

    #[error("Could not write to device.")]
    FailedToWriteToDevice,

    #[error("Unexpected error. Destination namespace was non empty but destination path is not a remote path.")]
    UnexpectedHostDestination,

    #[error("Could not create Regex pattern \"{pattern}\": {error}.")]
    FailedToCreateRegex { pattern: String, error: regex::Error },

    #[error("At least one path must be a remote path. {}", REMOTE_PATH_HELP)]
    NoRemotePaths,

    #[error(
        "Could not find an instance with the moniker: {moniker}\n\
    Use `ffx component list` or `ffx component show` to find the correct moniker of your instance."
    )]
    InstanceNotFound { moniker: String },

    #[error("Encountered an unexpected error when attempting to retrieve namespace with the provider moniker: {moniker}. {error:?}.")]
    UnexpectedErrorFromMoniker { moniker: String, error: fsys::RealmQueryError },

    #[error("Could not find file {file} in namespace.")]
    NamespaceFileNotFound { file: String },
}

// Constant used to compare the number of components within a path.
// For example, the path data/nested contains 2 components.
const NESTED_PATH_DEPTH: usize = 2;

/// Transfer files between a component's namespace to/from the host machine.
///
/// # Arguments
/// * `realm_query`: |RealmQueryProxy| to fetch the component's namespace.
/// * `paths`: The host and remote paths used for file copying.
/// * `verbose`: Flag used to indicate whether or not to print output to console.
pub async fn copy_cmd(
    realm_query: &fsys::RealmQueryProxy,
    mut paths: Vec<String>,
    verbose: bool,
) -> Result<()> {
    validate_paths(&paths)?;

    let mut namespaces: HashMap<String, fio::DirectoryProxy> = HashMap::new();
    // paths is safe to unwrap as validate_paths ensures that it is non-empty.
    let destination_path = paths.pop().unwrap();

    for source_path in paths {
        let result: Result<()> = match (
            HostOrRemotePath::parse(&source_path),
            HostOrRemotePath::parse(&destination_path),
        ) {
            (HostOrRemotePath::Remote(source), HostOrRemotePath::Host(destination)) => {
                let source_namespace = get_namespace_or_insert(
                    &realm_query,
                    source.clone().remote_id,
                    &mut namespaces,
                )
                .await?;

                let (paths, opened_namespace) =
                    normalize_paths(source.clone(), &source_namespace).await?;

                for remote_path in paths {
                    if is_remote_file(remote_path.clone(), &opened_namespace).await? {
                        copy_remote_file_to_host(
                            NamespacedPath { path: remote_path, ns: source_namespace.to_owned() },
                            destination.clone(),
                        )
                        .await?;

                        if verbose {
                            println!(
                                "Successfully copied {} to {}",
                                &source_path, &destination_path
                            );
                        }
                    } else if verbose {
                        //TODO(https://fxrev.dev/116065): add recursive flag for wildcards.
                        println!(
                            "Subdirectory \"{}\" ignored as recursive copying is currently not supported.",
                            remote_path.to_string()
                        );
                    }
                }
                Ok(())
            }

            (HostOrRemotePath::Remote(source), HostOrRemotePath::Remote(destination)) => {
                let source_namespace = get_namespace_or_insert(
                    &realm_query,
                    source.clone().remote_id,
                    &mut namespaces,
                )
                .await?;

                let destination_namespace = get_namespace_or_insert(
                    &realm_query,
                    destination.clone().remote_id,
                    &mut namespaces,
                )
                .await?;

                let (paths, opened_source) =
                    normalize_paths(source.clone(), &source_namespace).await?;

                for remote_path in paths {
                    if is_remote_file(remote_path.clone(), &opened_source).await? {
                        copy_remote_file_to_remote(
                            NamespacedPath { path: remote_path, ns: source_namespace.to_owned() },
                            NamespacedPath {
                                path: destination.clone(),
                                ns: destination_namespace.to_owned(),
                            },
                        )
                        .await?;

                        if verbose {
                            println!(
                                "Successfully copied {} to {}",
                                &source_path, &destination_path
                            );
                        }
                    } else if verbose {
                        //TODO(https://fxrev.dev/116065): add recursive flag for wildcards.
                        println!(
                            "Subdirectory \"{}\" ignored as recursive copying is currently not supported.",
                            remote_path.to_string()
                        );
                    }
                }

                Ok(())
            }

            (HostOrRemotePath::Host(source), HostOrRemotePath::Remote(destination)) => {
                let destination_namespace = get_namespace_or_insert(
                    &realm_query,
                    destination.clone().remote_id,
                    &mut namespaces,
                )
                .await?;

                copy_host_file_to_remote(
                    source,
                    NamespacedPath { path: destination, ns: destination_namespace.to_owned() },
                )
                .await?;

                if verbose {
                    println!("Successfully copied {} to {}", &source_path, &destination_path);
                }

                Ok(())
            }

            (HostOrRemotePath::Host(_), HostOrRemotePath::Host(_)) => {
                Err(CopyError::NoRemotePaths.into())
            }
        };

        match result {
            Ok(_) => continue,
            Err(e) => bail!(
                "Copy failed for source path: {} and destination path: {}. {}",
                &source_path,
                &destination_path,
                e
            ),
        };
    }

    Ok(())
}

pub async fn is_remote_file(source_path: RemotePath, namespace: &Directory) -> Result<bool> {
    let source_file = source_path.relative_path.file_name().map_or_else(
        || Err(CopyError::EmptyFileName),
        |file| Ok(file.to_string_lossy().to_string()),
    )?;
    let remote_entry = namespace.entry_if_exists(&source_file).await?;

    match remote_entry {
        Some(remote_destination) => {
            match remote_destination.kind {
                // TODO(https://fxrev.dev/745090): Update component_manager vfs to assign proper DirentKinds when installing the directory tree.
                DirentKind::File => Ok(true),
                _ => Ok(false),
            }
        }
        None => Err(CopyError::NamespaceFileNotFound { file: source_file }.into()),
    }
}

// Returns the normalized remote source path that may contain a wildcard, and
// the directory of the source file of a component's namespace.
// If the source contains a wildcard, the source is expanded to multiple paths.
// # Arguments
// * `source`: A wildcard path or path on a component's namespace.
// * `namespace`: The source path's namespace directory.
pub async fn normalize_paths(
    source: RemotePath,
    namespace: &fio::DirectoryProxy,
) -> Result<(Vec<RemotePath>, Directory)> {
    let directory = match &source.relative_path.parent() {
        Some(directory) => PathBuf::from(directory),
        None => {
            return Err(CopyError::NoParentFolder { path: source.relative_path_string() }.into())
        }
    };
    let namespace = Directory::from_proxy(namespace.to_owned())
        .open_dir(&directory, fio::OpenFlags::RIGHT_READABLE)?;

    if !&source.contains_wildcard() {
        return Ok((vec![source], namespace));
    }

    let file_pattern = &source
        .relative_path
        .file_name()
        .map_or_else(
            || Err(CopyError::EmptyFileName),
            |file| Ok(file.to_string_lossy().to_string()),
        )?
        .replace("*", ".*"); // Regex syntax requires a . before wildcard.

    let entries = get_matching_ns_entries(namespace.clone()?, file_pattern.clone()).await?;

    if entries.len() == 0 {
        return Err(CopyError::NoWildCardMatches { pattern: file_pattern.to_string() }.into());
    }

    let paths = entries
        .iter()
        .map(|file| {
            RemotePath::parse(&format!(
                "{}::/{}",
                &source.remote_id,
                directory.join(file).as_path().display().to_string()
            ))
        })
        .collect::<Result<Vec<RemotePath>>>()?;

    Ok((paths, namespace))
}

// Checks whether the hashmap contains the existing moniker and creates a new (moniker, DirectoryProxy) pair if it doesn't exist.
// # Arguments
// * `realm_query`: |RealmQueryProxy| to fetch the component's namespace.
// * `moniker`: A moniker used to retrieve a namespace directory.
// * `namespaces`: A table of monikers that map to namespace directories.
pub async fn get_namespace_or_insert(
    realm_query: &fsys::RealmQueryProxy,
    moniker: String,
    namespaces: &mut HashMap<String, fio::DirectoryProxy>,
) -> Result<fio::DirectoryProxy> {
    if !namespaces.contains_key(&moniker) {
        let namespace = retrieve_namespace(&realm_query, &moniker).await?;
        namespaces.insert(moniker.clone(), namespace);
    }

    Ok(namespaces.get(&moniker).unwrap().to_owned())
}

// Checks that the paths meet the following conditions:
// Destination path does not contain a wildcard.
// At least two paths are provided.
// # Arguments
// *`paths`: list of filepaths to be processed.
pub fn validate_paths(paths: &Vec<String>) -> Result<()> {
    if paths.len() < 2 {
        Err(CopyError::NotEnoughPaths.into())
    } else if paths.last().unwrap().contains("*") {
        Err(CopyError::DestinationContainWildcard.into())
    } else {
        Ok(())
    }
}

/// Retrieves the directory proxy associated with a component's namespace
/// # Arguments
/// * `realm_query`: |RealmQueryProxy| to retrieve a component instance.
/// * `moniker`: Absolute moniker of a component instance.
pub async fn retrieve_namespace(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<fio::DirectoryProxy> {
    // A relative moniker is required for |fuchsia.sys2/RealmQuery.GetInstanceInfo|
    let relative_moniker = format!(".{moniker}");
    let (_, resolved_state) = match realm_query.get_instance_info(&relative_moniker).await? {
        Ok((info, state)) => (info, state),
        Err(fsys::RealmQueryError::InstanceNotFound) => {
            return Err(CopyError::InstanceNotFound { moniker: moniker.to_string() }.into());
        }
        Err(e) => {
            return Err(CopyError::UnexpectedErrorFromMoniker {
                moniker: moniker.to_string(),
                error: e,
            }
            .into())
        }
    };
    // resolved_state is safe to unwrap as an error would be thrown otherwise in the above statement.
    let resolved_state = resolved_state.unwrap();
    let namespace = (*resolved_state).ns_dir.into_proxy()?;
    Ok(namespace)
}

/// Normalizes all paths in the namespace such that the namespace is always opened
/// at the parent of the provided path.
///
/// The namespace for paths with a depth of at least two will
/// have the parent of the path opened. For example, the path "data/nested"
/// will result in the "data" path to be opened already in the namespace.
///
/// # Arguments
/// * `namespace`: A proxy to a namespace directory.
/// * `path`: A path within a component's namespace.
pub fn normalize_namespace(namespace: fio::DirectoryProxy, path: RemotePath) -> Result<Directory> {
    if path.relative_path.components().count() >= NESTED_PATH_DEPTH {
        let directory = path.relative_path.parent().unwrap();
        Directory::from_proxy(namespace)
            .open_dir(&directory, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE)
    } else {
        Ok(Directory::from_proxy(namespace))
    }
}

/// Writes file contents from a directory to a component's namespace.
///
/// # Arguments
/// * `source`: The host filepath.
/// * `destination`: The path and proxy of a namespace directory.
pub async fn copy_host_file_to_remote(source: PathBuf, destination: NamespacedPath) -> Result<()> {
    let destination_namespace = Directory::from_proxy(destination.ns.to_owned());
    let destination_path = normalize_destination(
        &normalize_namespace(destination.ns, destination.path.clone())?,
        HostOrRemotePath::Host(source.clone()),
        HostOrRemotePath::Remote(destination.path.clone()),
    )
    .await?;

    let data = read(&source)?;
    destination_namespace
        .verify_directory_is_read_write(&destination_path.parent().unwrap())
        .await?;
    destination_namespace.create_file(destination_path, data.as_slice()).await?;

    Ok(())
}

/// Writes file contents to a directory from a component's namespace.
///
/// # Arguments
/// * `source`: The path and proxy of a namespace directory.
/// * `destination`: The host filepath.
pub async fn copy_remote_file_to_host(source: NamespacedPath, destination: PathBuf) -> Result<()> {
    let file_path = &source.path.relative_path.clone();
    let source_namespace = Directory::from_proxy(source.ns.to_owned());
    let destination_path = normalize_destination(
        &source_namespace,
        HostOrRemotePath::Remote(source.path),
        HostOrRemotePath::Host(destination),
    )
    .await?;

    let data = source_namespace.read_file_bytes(file_path).await?;
    write(destination_path, data).map_err(|e| CopyError::FailedToWriteToHost { error: e })?;

    Ok(())
}

/// Writes file contents to a component's namespace from a component's namespace.
///
/// # Arguments
/// * `source`: The path and proxy of a namespace directory.
/// * `destination`: The path and proxy of a namespace directory.
pub async fn copy_remote_file_to_remote(
    source: NamespacedPath,
    destination: NamespacedPath,
) -> Result<()> {
    let source_namespace = Directory::from_proxy(source.ns.to_owned());
    let destination_namespace = Directory::from_proxy(destination.ns.to_owned());
    let destination_path = normalize_destination(
        &normalize_namespace(destination.ns, destination.path.clone())?,
        HostOrRemotePath::Remote(source.path.clone()),
        HostOrRemotePath::Remote(destination.path),
    )
    .await?;

    let data = source_namespace.read_file_bytes(&source.path.relative_path).await?;
    destination_namespace
        .verify_directory_is_read_write(&destination_path.parent().unwrap())
        .await?;
    destination_namespace.create_file(destination_path, data.as_slice()).await?;
    Ok(())
}

// Retrieves all entries within a directory in a namespace containing a file pattern.
///
/// # Arguments
/// * `namespace`: A directory to a component's namespace.
/// * `file_pattern`: A file pattern to match in a component's directory.
pub async fn get_matching_ns_entries(
    namespace: Directory,
    file_pattern: String,
) -> Result<Vec<String>> {
    let mut entries = namespace.entry_names().await?;

    let file_pattern = Regex::new(format!(r"^{}$", file_pattern).as_str()).map_err(|e| {
        CopyError::FailedToCreateRegex { pattern: file_pattern.to_string(), error: e }
    })?;

    entries.retain(|file_name| file_pattern.is_match(file_name.as_str()));

    Ok(entries)
}

// Duplicates the client end of a namespace directory.
///
/// # Arguments
/// * `ns_dir`: A proxy to the component's namespace directory.
pub fn duplicate_namespace_client(ns_dir: &fio::DirectoryProxy) -> Result<fio::DirectoryProxy> {
    let (client, server) = create_endpoints::<fio::NodeMarker>().unwrap();
    ns_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server).unwrap();
    let client =
        ClientEnd::<fio::DirectoryMarker>::new(client.into_channel()).into_proxy().unwrap();
    Ok(client)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_utils::{
            generate_directory_paths, generate_file_paths, populate_host_with_file_contents,
            read_data_from_namespace, serve_realm_query_deprecated,
            serve_realm_query_with_namespace, File, SeedPath,
        },
        fidl::endpoints::{create_endpoints, create_proxy, ClientEnd, Proxy},
        fidl_fuchsia_io as fio,
        std::{iter::zip, path::Path},
        tempfile::tempdir,
        test_case::test_case,
    };

    const FOO_MONIKER: &'static str = "/foo/bar";
    const RELATIVE_FOO_MONIKER: &'static str = "./foo/bar";
    const BAR_MONIKER: &'static str = "/bar/foo";
    const RELATIVE_BAR_MONIKER: &'static str = "./bar/foo";
    const CHANNEL_SIZE_LIMIT: u64 = 64 * 1024;
    const LARGE_FILE_ARRAY: [u8; CHANNEL_SIZE_LIMIT as usize] = [b'a'; CHANNEL_SIZE_LIMIT as usize];
    const OVER_LIMIT_FILE_ARRAY: [u8; (CHANNEL_SIZE_LIMIT + 1) as usize] =
        [b'a'; (CHANNEL_SIZE_LIMIT + 1) as usize];
    const READ_WRITE: bool = false;
    const READ_ONLY: bool = true;

    // We can call from_utf8_unchecked as the file arrays only contain the character 'a' which is safe to unwrap.
    const LARGE_FILE_DATA: &str = unsafe { std::str::from_utf8_unchecked(&LARGE_FILE_ARRAY) };
    const OVER_LIMIT_FILE_DATA: &str =
        unsafe { std::str::from_utf8_unchecked(&OVER_LIMIT_FILE_ARRAY) };

    struct Component {
        name: &'static str,
        moniker: &'static str,
        seed_files: Vec<SeedPath>,
    }

    #[derive(Clone)]
    struct Input {
        source: String,
        destination: String,
    }

    #[derive(Clone)]
    struct Inputs {
        sources: Vec<String>,
        destination: String,
    }

    #[derive(Clone)]
    struct Expectation {
        path: &'static str,
        data: &'static str,
    }

    fn create_resolved_state(
        exposed_dir: ClientEnd<fio::DirectoryMarker>,
        ns_dir: ClientEnd<fio::DirectoryMarker>,
    ) -> Option<Box<fsys::ResolvedState>> {
        Some(Box::new(fsys::ResolvedState {
            uses: vec![],
            exposes: vec![],
            config: None,
            pkg_dir: None,
            execution: Some(Box::new(fsys::ExecutionState {
                out_dir: None,
                runtime_dir: None,
                start_reason: "Debugging Workflow".to_string(),
            })),
            exposed_dir,
            ns_dir,
        }))
    }

    fn create_hashmap_of_instance_info(
        name: &str,
        moniker: &str,
        ns_dir: ClientEnd<fio::DirectoryMarker>,
    ) -> HashMap<String, (fsys::InstanceInfo, Option<Box<fsys::ResolvedState>>)> {
        let (exposed_dir, _) = create_endpoints::<fio::DirectoryMarker>().unwrap();
        HashMap::from([(
            name.to_string(),
            (
                fsys::InstanceInfo {
                    moniker: moniker.to_string(),
                    url: String::new(),
                    instance_id: None,
                    state: fsys::InstanceState::Started,
                },
                create_resolved_state(exposed_dir, ns_dir),
            ),
        )])
    }

    fn create_realm_query(seed_files: Vec<SeedPath>, is_read_only: bool) -> fsys::RealmQueryProxy {
        let (ns_dir, ns_server) = create_endpoints::<fio::DirectoryMarker>().unwrap();
        let () = serve_realm_query_with_namespace(ns_server, seed_files, is_read_only).unwrap();
        let query_instance =
            create_hashmap_of_instance_info(RELATIVE_FOO_MONIKER, RELATIVE_FOO_MONIKER, ns_dir);
        serve_realm_query_deprecated(query_instance)
    }

    fn create_realm_query_with_ns_client(
        seed_files: Vec<SeedPath>,
        is_read_only: bool,
    ) -> (fsys::RealmQueryProxy, fio::DirectoryProxy) {
        let (ns_dir, ns_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let dup_client = duplicate_namespace_client(&ns_dir).unwrap();
        let () = serve_realm_query_with_namespace(ns_server, seed_files, is_read_only).unwrap();
        let ns_dir = ClientEnd::<fio::DirectoryMarker>::new(ns_dir.into_channel().unwrap().into());
        let query_instance =
            create_hashmap_of_instance_info(RELATIVE_FOO_MONIKER, RELATIVE_FOO_MONIKER, ns_dir);
        let realm_query = serve_realm_query_deprecated(query_instance);

        (realm_query, dup_client)
    }

    fn create_realm_query_with_multiple_components(
        components: Vec<Component>,
        is_read_only: bool,
    ) -> (fsys::RealmQueryProxy, fio::DirectoryProxy) {
        let mut instances = HashMap::new();
        let mut proxies = Vec::new();

        for component in components {
            let (ns_dir, ns_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
            let dup_client = duplicate_namespace_client(&ns_dir).unwrap();

            let () =
                serve_realm_query_with_namespace(ns_server, component.seed_files, is_read_only)
                    .unwrap();
            proxies.push(dup_client);
            let ns_dir =
                ClientEnd::<fio::DirectoryMarker>::new(ns_dir.into_channel().unwrap().into());
            let query_instance =
                create_hashmap_of_instance_info(component.name, component.moniker, ns_dir);
            instances.extend(query_instance);
        }

        let realm_query = serve_realm_query_deprecated(instances);
        (realm_query, proxies.last().unwrap().to_owned())
    }

    fn path_for_foo_moniker(path: &str) -> String {
        format!("{}::{}", FOO_MONIKER, path)
    }

    fn path_for_bar_moniker(path: &str) -> String {
        format!("{}::{}", BAR_MONIKER, path)
    }

    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/foo.txt", data: "Hello"}; "single_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: true, name: "/foo.txt", data: "World"}]),
                Expectation{path: "/foo.txt", data: "Hello"}; "overwrite_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/bar.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/bar.txt", data: "Hello"}; "different_file_name")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/foo.txt", data: "Hello"}; "infer_path")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/foo.txt", data: "Hello"}; "infer_path_slash")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}]),
                Expectation{path: "/foo.txt", data: "Hello"}; "populated_directory")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: LARGE_FILE_DATA}]),
                Expectation{path: "/foo.txt", data: LARGE_FILE_DATA}; "large_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: OVER_LIMIT_FILE_DATA}]),
                Expectation{path: "/foo.txt", data: OVER_LIMIT_FILE_DATA}; "over_limit_file")]
    #[fuchsia::test]
    async fn copy_device_to_host(
        input: Input,
        seed_files: Vec<SeedPath>,
        expectation: Expectation,
    ) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let destination_path = format!("{}{}", root_path, input.destination);
        populate_host_with_file_contents(&root_path, seed_files.clone()).unwrap();
        let realm_query = create_realm_query(seed_files, READ_ONLY);

        copy_cmd(&realm_query, vec![input.source, destination_path], /*verbose=*/ false)
            .await
            .unwrap();

        let expected_data = expectation.data.to_owned().into_bytes();
        let actual_data_path_string = format!("{}{}", root_path, expectation.path);
        let actual_data_path = Path::new(&actual_data_path_string);
        let actual_data = read(actual_data_path).unwrap();
        assert_eq!(actual_data, expected_data);
    }

    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "all_matches")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: true, name: "/foo.txt", data: "World"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "all_matches_overwrite")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "/data/nested/foo.txt", data: "World"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "all_matches_nested")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "file_extension")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.*"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "file_extension_2")]
    #[test_case(Input{source: path_for_foo_moniker("/data/fo*.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}]; "file_substring_match")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: "/".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}, Expectation{path: "/bar.txt", data: "World"}]; "multi_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*fo*.txt"), destination: "/".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/foobar.txt", data: "World"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}, Expectation{path: "/foobar.txt", data: "World"}]; "multi_wildcard")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: "/".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/foobar.txt", data: "World"},
                     File{on_host: true, name: "/foo.txt", data: "World"}, File{on_host: true , name: "/foobar.txt", data: "Hello"}]),
                vec![Expectation{path: "/foo.txt", data: "Hello"}, Expectation{path: "/foobar.txt", data: "World"}]; "multi_file_overwrite")]
    #[fuchsia::test]
    async fn copy_device_to_host_wildcard(
        input: Input,
        seed_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let destination_path = format!("{}{}", root_path, input.destination);
        populate_host_with_file_contents(&root_path, seed_files.clone()).unwrap();
        let realm_query = create_realm_query(seed_files, READ_ONLY);

        copy_cmd(&realm_query, vec![input.source, destination_path], /*verbose=*/ true)
            .await
            .unwrap();

        for expected in expectation {
            let expected_data = expected.data.to_owned().into_bytes();
            let actual_data_path_string = format!("{}{}", &root_path, expected.path);
            let actual_data = read(Path::new(&actual_data_path_string)).unwrap();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: "wrong_moniker/foo/bar::/data/foo.txt".to_string(), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]); "bad_moniker")]
    #[test_case(Input{source: path_for_foo_moniker("/data/bar.txt"), destination: "/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]); "bad_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: "/bar/foo.txt".to_string()},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]); "bad_directory")]
    #[fuchsia::test]
    async fn copy_device_to_host_fails(input: Input, seed_files: Vec<SeedPath>) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let destination_path = format!("{}{}", root_path, input.destination);
        let realm_query = create_realm_query(seed_files, READ_ONLY);
        let result =
            copy_cmd(&realm_query, vec![input.source, destination_path], /*verbose=*/ true).await;

        assert!(result.is_err());
    }

    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/foo.txt")},
                generate_directory_paths(vec!["/data"]),
                Expectation{path: "/data/foo.txt", data: "Hello"}; "single_file")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/bar.txt")},
                generate_directory_paths(vec!["/data"]),
                Expectation{path: "/data/bar.txt", data: "Hello"}; "different_file_name")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/foo.txt")},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "World"}]),
                Expectation{path: "/data/foo.txt", data: "Hello"}; "overwrite_file")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data")},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/data/foo.txt", data: "Hello"}; "infer_path")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/")},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "/data/foo.txt", data: "Hello"}; "infer_slash_path")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/nested/foo.txt")},
                generate_directory_paths(vec!["/data", "/data/nested"]),
                Expectation{path: "/data/nested/foo.txt", data: "Hello"}; "nested_path")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/nested")},
                generate_directory_paths(vec!["/data", "/data/nested"]),
                Expectation{path: "/data/nested/foo.txt", data: "Hello"}; "infer_nested_path")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/")},
                generate_directory_paths(vec!["/data"]),
                Expectation{path: "/data/foo.txt", data: LARGE_FILE_DATA}; "large_file")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/")},
                generate_directory_paths(vec!["/data"]),
                Expectation{path: "/data/foo.txt", data: OVER_LIMIT_FILE_DATA}; "over_channel_limit_file")]
    #[fuchsia::test]
    async fn copy_host_to_device(
        input: Input,
        seed_files: Vec<SeedPath>,
        expectation: Expectation,
    ) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let source_path = format!("{}{}", root_path, &input.source);
        write(&source_path, expectation.data.to_owned().into_bytes()).unwrap();
        let (realm_query, ns_dir) = create_realm_query_with_ns_client(seed_files, READ_WRITE);

        copy_cmd(&realm_query, vec![source_path, input.destination], /*verbose=*/ false)
            .await
            .unwrap();

        let actual_data = read_data_from_namespace(&ns_dir, expectation.path).await.unwrap();
        let expected_data = expectation.data.to_owned().into_bytes();
        assert_eq!(actual_data, expected_data);
    }

    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/data/foo.txt")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}]; "single_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/data/nested/foo.txt")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data", "data/nested"])}],
                vec![Expectation{path: "/data/nested/foo.txt", data: "Hello"}]; "nested")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/data/bar.txt")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/bar.txt", data: "Hello"}]; "different_file_name")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/data/foo.txt")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}]; "overwrite_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_foo_moniker("/data/foo.txt")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}]; "same_component")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: path_for_bar_moniker("/data")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "wildcard_match_all_multi_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*.txt"), destination: path_for_bar_moniker("/data")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "wildcard_match_files_extensions_multi_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/*"), destination: path_for_bar_moniker("/data")},
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "World"}, File{on_host: false, name: "data/bar.txt", data: "Hello"}])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "wildcard_match_all_multi_file_overwrite")]
    #[fuchsia::test]
    async fn copy_device_to_device(
        input: Input,
        components: Vec<Component>,
        expectation: Vec<Expectation>,
    ) {
        let (realm_query, destination_namespace) =
            create_realm_query_with_multiple_components(components, READ_WRITE);

        copy_cmd(&realm_query, vec![input.source, input.destination], /*verbose=*/ false)
            .await
            .unwrap();

        for expected in expectation {
            let actual_data =
                read_data_from_namespace(&destination_namespace, expected.path).await.unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: path_for_foo_moniker("/data/"), destination: path_for_bar_moniker("/data/foo.txt")}; "no_source_file")]
    #[test_case(Input{source: path_for_foo_moniker("/data/cat.txt"), destination: path_for_bar_moniker("/data/foo.txt")}; "bad_file")]
    #[test_case(Input{source: path_for_foo_moniker("/foo.txt"), destination: path_for_bar_moniker("/data/foo.txt")}; "bad_source_folder")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/file.txt")}; "bad_destination_folder")]
    #[test_case(Input{source: "/hello/world::/data/foo.txt".to_string(), destination: path_for_bar_moniker("/data/file.txt")}; "bad_source_moniker")]
    #[test_case(Input{source: path_for_foo_moniker("/data/foo.txt"), destination: path_for_bar_moniker("/data/file.txt")}; "bad_destination_moniker")]
    #[fuchsia::test]
    async fn copy_device_to_device_fails(input: Input) {
        let components = vec![
            Component {
                name: RELATIVE_FOO_MONIKER,
                moniker: RELATIVE_FOO_MONIKER,
                seed_files: generate_file_paths(vec![
                    File { on_host: false, name: "data/foo.txt", data: "Hello" },
                    File { on_host: false, name: "data/bar.txt", data: "World" },
                ]),
            },
            Component {
                name: RELATIVE_FOO_MONIKER,
                moniker: FOO_MONIKER,
                seed_files: generate_directory_paths(vec!["data"]),
            },
        ];
        let (realm_query, _) = create_realm_query_with_multiple_components(components, READ_WRITE);

        let result =
            copy_cmd(&realm_query, vec![input.source, input.destination], /*verbose=*/ false).await;

        assert!(result.is_err());
    }

    #[test_case(Inputs{sources: vec!["/foo.txt".to_string()], destination: path_for_foo_moniker("/data/")},
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}]; "single_file_wildcard")]
    #[test_case(Inputs{sources: vec!["/foo.txt".to_string()], destination: path_for_foo_moniker("/data/")},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "World"}]),
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}]; "single_file_wildcard_overwrite")]
    #[test_case(Inputs{sources: vec!["/foo.txt".to_string(), "/bar.txt".to_string()], destination: path_for_foo_moniker("/data/")},
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "multi_file_wildcard")]
    #[test_case(Inputs{sources: vec!["/foo.txt".to_string(), "/bar.txt".to_string()], destination: path_for_foo_moniker("/data/")},
                generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "World"}, File{on_host: false, name: "data/bar.txt", data: "World"}]),
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "multi_wildcard_file_overwrite")]
    #[fuchsia::test]
    async fn copy_host_to_device_wildcard(
        input: Inputs,
        seed_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();

        for (path, expected) in zip(input.sources.clone(), expectation.clone()) {
            let source_path = format!("{}{}", root_path, path);
            write(&source_path, expected.data.to_owned().into_bytes()).unwrap();
        }

        let (realm_query, ns_dir) = create_realm_query_with_ns_client(seed_files, READ_WRITE);
        let mut paths: Vec<String> =
            input.sources.into_iter().map(|path| format!("{}{}", root_path, path)).collect();
        paths.push(input.destination.to_string());

        copy_cmd(&realm_query, paths, /*verbose=*/ false).await.unwrap();

        for expected in expectation {
            let actual_data = read_data_from_namespace(&ns_dir, expected.path).await.unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/foo.txt")}; "root_dir")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("")}; "root_dir_infer_path")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/")}; "root_dir_infer_path_slash")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: "wrong_moniker/foo/bar::/data/foo.txt".to_string()}; "bad_moniker")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("//bar/foo.txt")}; "bad_directory")]
    #[fuchsia::test]
    async fn copy_host_to_device_fails(input: Input) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let source_path = format!("{}{}", root_path, input.source);
        write(&source_path, "Hello".to_owned().into_bytes()).unwrap();
        let realm_query = create_realm_query(generate_directory_paths(vec!["data"]), READ_WRITE);

        let result =
            copy_cmd(&realm_query, vec![source_path, input.destination], /*verbose=*/ false).await;

        assert!(result.is_err());
    }

    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/foo.txt")}; "file_in_read_only_dir")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data")}; "infer_path_read_only_dir")]
    #[test_case(Input{source: "/foo.txt".to_string(), destination: path_for_foo_moniker("/data/")}; "infer_path_slash_read_only_dir")]
    #[fuchsia::test]
    async fn copy_host_to_device_fails_read_only(input: Input) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        let source_path = format!("{}{}", root_path, input.source);
        write(&source_path, "Hello".to_owned().into_bytes()).unwrap();
        let realm_query = create_realm_query(generate_directory_paths(vec!["data"]), READ_ONLY);

        let result =
            copy_cmd(&realm_query, vec![source_path, input.destination], /*verbose=*/ false).await;

        assert!(result.is_err());
    }

    #[test_case(vec![]; "no_wildcard_matches")]
    #[test_case(vec!["/foo.txt".to_string()]; "not_enough_args")]
    #[test_case(vec![path_for_foo_moniker("/data/*"), path_for_foo_moniker("/data/*")]; "remote_wildcard_destination")]
    #[test_case(vec![path_for_foo_moniker("/data/*"), path_for_foo_moniker("/data/*"), "/".to_string()]; "multi_wildcards_remote")]
    #[test_case(vec!["/*".to_string(), "/*".to_string()]; "host_wildcard_destination")]
    #[fuchsia::test]
    async fn copy_wildcard_fails(paths: Vec<String>) {
        let (realm_query, _) = create_realm_query_with_ns_client(
            generate_file_paths(vec![File { on_host: false, name: "data/foo.txt", data: "Hello" }]),
            READ_WRITE,
        );
        let result = copy_cmd(&realm_query, paths, /*verbose=*/ false).await;

        assert!(result.is_err());
    }

    #[test_case(Inputs{sources: vec![path_for_foo_moniker("/data/foo.txt"), "/bar.txt".to_string()], destination: path_for_bar_moniker("/data/")},
                generate_file_paths(vec![File{on_host: true, name: "/bar.txt", data: "World"}]),
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}]; "no_wildcard_mix")]
    #[test_case(Inputs{sources: vec![path_for_foo_moniker("/data/foo.txt"), path_for_foo_moniker("/data/*"), "/foobar.txt".to_string()], destination: path_for_bar_moniker("/data/")},
                generate_file_paths(vec![File{on_host: true, name: "/foobar.txt", data: "World"}]),
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}, Expectation{path: "/data/foobar.txt", data: "World"}]; "wildcard_mix")]
    #[test_case(Inputs{sources: vec![path_for_foo_moniker("/data/*"), path_for_foo_moniker("/data/*"), "/foobar.txt".to_string()], destination: path_for_bar_moniker("/data/")},
                generate_file_paths(vec![File{on_host: true, name: "/foobar.txt", data: "World"}]),
                vec![Component{name: RELATIVE_FOO_MONIKER, moniker: RELATIVE_FOO_MONIKER, seed_files: generate_file_paths(vec![File{on_host: false, name: "data/foo.txt", data: "Hello"}, File{on_host: false, name: "data/bar.txt", data: "World"}])},
                     Component{name: RELATIVE_BAR_MONIKER, moniker: RELATIVE_BAR_MONIKER, seed_files: generate_directory_paths(vec!["data"])}],
                vec![Expectation{path: "/data/foo.txt", data: "Hello"}, Expectation{path: "/data/bar.txt", data: "World"}, Expectation{path: "/data/foobar.txt", data: "World"}]; "double_wildcard")]
    #[fuchsia::test]
    async fn copy_mixed_tests_remote_destination(
        input: Inputs,
        seed_files: Vec<SeedPath>,
        components: Vec<Component>,
        expectation: Vec<Expectation>,
    ) {
        let root = tempdir().unwrap();
        let root_path = root.path().to_str().unwrap();
        populate_host_with_file_contents(&root_path, seed_files.clone()).unwrap();
        let (realm_query, destination_namespace) =
            create_realm_query_with_multiple_components(components, READ_WRITE);
        let mut paths: Vec<String> = input
            .sources
            .clone()
            .into_iter()
            .map(|path| match HostOrRemotePath::parse(&path) {
                HostOrRemotePath::Remote(_) => path.to_string(),
                HostOrRemotePath::Host(_) => format!("{}{}", root_path, path),
            })
            .collect();
        paths.push(input.destination.to_owned());

        copy_cmd(&realm_query, paths, /*verbose=*/ false).await.unwrap();

        for expected in expectation {
            let actual_data =
                read_data_from_namespace(&destination_namespace, expected.path).await.unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }
}
