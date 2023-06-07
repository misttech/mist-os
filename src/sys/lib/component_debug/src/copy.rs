// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::create_proxy;

use {
    crate::{
        io::{Directory, DirentKind, LocalDirectory, RemoteDirectory},
        path::{
            add_source_filename_to_path_if_absent, open_parent_subdir_readable, LocalOrRemotePath,
        },
    },
    anyhow::{bail, Result},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    regex::Regex,
    std::{collections::HashMap, path::PathBuf},
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum CopyError {
    #[error("Destination can not have a wildcard.")]
    DestinationContainWildcard,

    #[error("At least two paths (local or remote) must be provided.")]
    NotEnoughPaths,

    #[error("File name was unexpectedly empty.")]
    EmptyFileName,

    #[error("Path does not contain a parent folder.")]
    NoParentFolder { path: String },

    #[error("No files found matching: {pattern}.")]
    NoWildCardMatches { pattern: String },

    #[error(
        "Could not find an instance with the moniker: {moniker}\n\
    Use `ffx component list` or `ffx component show` to find the correct moniker of your instance."
    )]
    InstanceNotFound { moniker: String },

    #[error("Encountered an unexpected error when attempting to retrieve namespace with the provider moniker: {moniker}. {error:?}.")]
    UnexpectedErrorFromMoniker { moniker: String, error: fsys::OpenError },

    #[error("No file found at {file} in remote component namespace.")]
    NamespaceFileNotFound { file: String },
}

/// Transfer files between a component's namespace to/from the local filesystem.
///
/// # Arguments
/// * `realm_query`: |RealmQueryProxy| to fetch the component's namespace.
/// * `paths`: The local and remote paths to copy. The last entry is the destination.
/// * `verbose`: Flag used to indicate whether or not to print output to console.
pub async fn copy_cmd<W: std::io::Write>(
    realm_query: &fsys::RealmQueryProxy,
    mut paths: Vec<String>,
    verbose: bool,
    mut writer: W,
) -> Result<()> {
    validate_paths(&paths)?;

    let mut namespace_dir_cache: HashMap<String, fio::DirectoryProxy> = HashMap::new();
    // paths is safe to unwrap as validate_paths ensures that it is non-empty.
    let destination_path = paths.pop().unwrap();

    for source_path in paths {
        let result: Result<()> = match (
            LocalOrRemotePath::parse(&source_path),
            LocalOrRemotePath::parse(&destination_path),
        ) {
            (LocalOrRemotePath::Remote(source), LocalOrRemotePath::Local(destination_path)) => {
                let source_dir = RemoteDirectory::from_proxy(
                    get_or_cache_namespace_dir_for_moniker(
                        &realm_query,
                        &source.remote_id,
                        &mut namespace_dir_cache,
                    )
                    .await?,
                );
                let destination_dir = LocalDirectory::new();

                do_copy(
                    &source_dir,
                    &source.relative_path,
                    &destination_dir,
                    &destination_path,
                    verbose,
                    &mut writer,
                )
                .await
            }

            (LocalOrRemotePath::Local(source_path), LocalOrRemotePath::Remote(destination)) => {
                let source_dir = LocalDirectory::new();
                let destination_dir = RemoteDirectory::from_proxy(
                    get_or_cache_namespace_dir_for_moniker(
                        &realm_query,
                        &destination.remote_id,
                        &mut namespace_dir_cache,
                    )
                    .await?,
                );

                do_copy(
                    &source_dir,
                    &source_path,
                    &destination_dir,
                    &destination.relative_path,
                    verbose,
                    &mut writer,
                )
                .await
            }

            (LocalOrRemotePath::Remote(source), LocalOrRemotePath::Remote(destination)) => {
                let source_dir = RemoteDirectory::from_proxy(
                    get_or_cache_namespace_dir_for_moniker(
                        &realm_query,
                        &source.remote_id,
                        &mut namespace_dir_cache,
                    )
                    .await?,
                );

                let destination_dir = RemoteDirectory::from_proxy(
                    get_or_cache_namespace_dir_for_moniker(
                        &realm_query,
                        &destination.remote_id,
                        &mut namespace_dir_cache,
                    )
                    .await?,
                );

                do_copy(
                    &source_dir,
                    &source.relative_path,
                    &destination_dir,
                    &destination.relative_path,
                    verbose,
                    &mut writer,
                )
                .await
            }

            (LocalOrRemotePath::Local(source_path), LocalOrRemotePath::Local(destination_path)) => {
                let source_dir = LocalDirectory::new();
                let destination_dir = LocalDirectory::new();
                do_copy(
                    &source_dir,
                    &source_path,
                    &destination_dir,
                    &destination_path,
                    verbose,
                    &mut writer,
                )
                .await
            }
        };

        match result {
            Ok(_) => continue,
            Err(e) => bail!("Copy from {} to {} failed: {}", &source_path, &destination_path, e),
        };
    }

    Ok(())
}

async fn do_copy<S: Directory, D: Directory, W: std::io::Write>(
    source_dir: &S,
    source_path: &PathBuf,
    destination_dir: &D,
    destination_path: &PathBuf,
    verbose: bool,
    writer: &mut W,
) -> Result<()> {
    let source_paths = maybe_expand_wildcards(source_path, source_dir).await?;
    for path in source_paths {
        if is_file(source_dir, &path).await? {
            let destination_path_path =
                add_source_filename_to_path_if_absent(destination_dir, &path, &destination_path)
                    .await?;

            let data = source_dir.read_file_bytes(path).await?;
            destination_dir.write_file(destination_path_path.clone(), &data).await?;

            if verbose {
                writeln!(
                    writer,
                    "Copied {} -> {}",
                    source_path.display(),
                    destination_path_path.display()
                )?;
            }
        } else {
            // TODO(https://fxbug.dev/116065): add recursive copy support.
            writeln!(
                writer,
                "Directory \"{}\" ignored as recursive copying is unsupported. (See fxbug.dev/116065)",
                path.display()
            )?;
        }
    }

    Ok(())
}

async fn is_file<D: Directory>(dir: &D, path: &PathBuf) -> Result<bool> {
    let parent_dir = open_parent_subdir_readable(path, dir)?;
    let source_file = path.file_name().map_or_else(
        || Err(CopyError::EmptyFileName),
        |file| Ok(file.to_string_lossy().to_string()),
    )?;

    let remote_type = parent_dir.entry_type(&source_file).await?;
    match remote_type {
        Some(kind) => match kind {
            DirentKind::File => Ok(true),
            _ => Ok(false),
        },
        None => Err(CopyError::NamespaceFileNotFound { file: source_file }.into()),
    }
}

/// If `path` contains a wildcard, returns the expanded list of files. Otherwise,
/// returns a list with a single entry.
///
/// # Arguments
///
/// * `path`: A path that may contain a wildcard.
/// * `dir`: Directory proxy to query to expand wildcards.
async fn maybe_expand_wildcards<D: Directory>(path: &PathBuf, dir: &D) -> Result<Vec<PathBuf>> {
    if !&path.to_string_lossy().contains("*") {
        return Ok(vec![path.clone()]);
    }
    let parent_dir = open_parent_subdir_readable(path, dir)?;

    let file_pattern = &path
        .file_name()
        .map_or_else(
            || Err(CopyError::EmptyFileName),
            |file| Ok(file.to_string_lossy().to_string()),
        )?
        .replace("*", ".*"); // Regex syntax requires a . before wildcard.

    let entries = get_dirents_matching_pattern(&parent_dir, file_pattern.clone()).await?;

    if entries.len() == 0 {
        return Err(CopyError::NoWildCardMatches { pattern: file_pattern.to_string() }.into());
    }

    let parent_dir_path = match path.parent() {
        Some(parent) => PathBuf::from(parent),
        None => {
            return Err(
                CopyError::NoParentFolder { path: path.to_string_lossy().to_string() }.into()
            )
        }
    };
    Ok(entries.iter().map(|file| parent_dir_path.join(file)).collect::<Vec<_>>())
}

/// Checks whether the hashmap contains the existing moniker and creates a new (moniker, DirectoryProxy) pair if it doesn't exist.
///
/// # Arguments
///
/// * `realm_query`: |RealmQueryProxy| to fetch the component's namespace.
/// * `moniker`: A moniker used to retrieve a namespace directory.
/// * `namespace_dir_cache`: A table of monikers that map to namespace directories.
async fn get_or_cache_namespace_dir_for_moniker(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &String,
    namespace_dir_cache: &mut HashMap<String, fio::DirectoryProxy>,
) -> Result<fio::DirectoryProxy> {
    if !namespace_dir_cache.contains_key(moniker) {
        let namespace = open_namespace_dir_for_moniker(&realm_query, moniker).await?;
        namespace_dir_cache.insert(moniker.clone(), namespace);
    }

    Ok(namespace_dir_cache.get(moniker).unwrap().to_owned())
}

/// Checks that the paths meet the following conditions:
///
/// * Destination path does not contain a wildcard.
/// * At least two path arguments are provided.
///
/// # Arguments
///
/// *`paths`: list of filepaths to be processed.
fn validate_paths(paths: &Vec<String>) -> Result<()> {
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
async fn open_namespace_dir_for_moniker(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<fio::DirectoryProxy> {
    // A relative moniker is required for |fuchsia.sys2/RealmQuery.GetInstanceInfo|
    let relative_moniker = format!(".{moniker}");
    let (namespace, server_end) = create_proxy::<fio::DirectoryMarker>()?;
    let server_end = ServerEnd::new(server_end.into_channel());
    match realm_query
        .open(
            &relative_moniker,
            fsys::OpenDirType::NamespaceDir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await?
    {
        Ok(()) => Ok(namespace),
        Err(fsys::OpenError::InstanceNotFound) => {
            Err(CopyError::InstanceNotFound { moniker: moniker.to_string() }.into())
        }
        Err(e) => {
            Err(CopyError::UnexpectedErrorFromMoniker { moniker: moniker.to_string(), error: e }
                .into())
        }
    }
}

// Retrieves all entries within a remote directory containing a file pattern.
///
/// # Arguments
/// * `dir`: A directory.
/// * `file_pattern`: A file pattern to match.
async fn get_dirents_matching_pattern<D: Directory>(
    dir: &D,
    file_pattern: String,
) -> Result<Vec<String>> {
    let mut entries = dir.entry_names().await?;

    let file_pattern = Regex::new(format!(r"^{}$", file_pattern).as_str())?;

    entries.retain(|file_name| file_pattern.is_match(file_name.as_str()));

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_utils::{
            create_tmp_dir, generate_directory_paths, generate_file_paths, serve_realm_query, File,
            SeedPath,
        },
        std::fs::{read, write},
        std::iter::zip,
        test_case::test_case,
    };

    const CHANNEL_SIZE_LIMIT: u64 = 64 * 1024;
    const LARGE_FILE_ARRAY: [u8; CHANNEL_SIZE_LIMIT as usize] = [b'a'; CHANNEL_SIZE_LIMIT as usize];
    const OVER_LIMIT_FILE_ARRAY: [u8; (CHANNEL_SIZE_LIMIT + 1) as usize] =
        [b'a'; (CHANNEL_SIZE_LIMIT + 1) as usize];

    // We can call from_utf8_unchecked as the file arrays only contain the character 'a' which is safe to unwrap.
    const LARGE_FILE_DATA: &str = unsafe { std::str::from_utf8_unchecked(&LARGE_FILE_ARRAY) };
    const OVER_LIMIT_FILE_DATA: &str =
        unsafe { std::str::from_utf8_unchecked(&OVER_LIMIT_FILE_ARRAY) };

    #[derive(Clone)]
    struct Input {
        source: &'static str,
        destination: &'static str,
    }

    #[derive(Clone)]
    struct Inputs {
        sources: Vec<&'static str>,
        destination: &'static str,
    }

    #[derive(Clone)]
    struct Expectation {
        path: &'static str,
        data: &'static str,
    }

    fn create_realm_query(
        foo_files: Vec<SeedPath>,
        bar_files: Vec<SeedPath>,
    ) -> (fsys::RealmQueryProxy, PathBuf, PathBuf) {
        let foo_ns_dir = create_tmp_dir(foo_files).unwrap();
        let bar_ns_dir = create_tmp_dir(bar_files).unwrap();
        let foo_path = foo_ns_dir.path().to_path_buf();
        let bar_path = bar_ns_dir.path().to_path_buf();
        let query = serve_realm_query(
            vec![],
            HashMap::new(),
            HashMap::new(),
            HashMap::from([
                (("./foo/bar".to_string(), fsys::OpenDirType::NamespaceDir), foo_ns_dir),
                (("./bar/foo".to_string(), fsys::OpenDirType::NamespaceDir), bar_ns_dir),
            ]),
        );
        (query, foo_path, bar_path)
    }

    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "foo.txt", data: "Hello"}; "single_file")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "foo.txt", data: "Hello"}; "overwrite_file")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "bar.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "bar.txt", data: "Hello"}; "different_file_name")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: ""},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "foo.txt", data: "Hello"}; "infer_path")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "./"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "foo.txt", data: "Hello"}; "infer_path_slash")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                Expectation{path: "foo.txt", data: "Hello"}; "populated_directory")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: LARGE_FILE_DATA}]),
                Expectation{path: "foo.txt", data: LARGE_FILE_DATA}; "large_file")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: OVER_LIMIT_FILE_DATA}]),
                Expectation{path: "foo.txt", data: OVER_LIMIT_FILE_DATA}; "over_limit_file")]
    #[fuchsia::test]
    async fn copy_device_to_local(
        input: Input,
        foo_files: Vec<SeedPath>,
        expectation: Expectation,
    ) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        let (realm_query, _, _) = create_realm_query(foo_files, vec![]);
        let destination_path = local_path.join(input.destination).display().to_string();

        eprintln!("Destination path: {:?}", destination_path);

        copy_cmd(
            &realm_query,
            vec![input.source.to_string(), destination_path],
            /*verbose=*/ false,
            std::io::stdout(),
        )
        .await
        .unwrap();

        let expected_data = expectation.data.to_owned().into_bytes();
        let actual_data_path = local_path.join(expectation.path);
        let actual_data = read(actual_data_path).unwrap();
        assert_eq!(actual_data, expected_data);
    }

    #[test_case(Input{source: "/foo/bar::/data/*", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "all_matches")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "foo.txt", data: "World"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "all_matches_overwrite")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/nested/foo.txt", data: "World"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "all_matches_nested")]
    #[test_case(Input{source: "/foo/bar::/data/*.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "file_extension")]
    #[test_case(Input{source: "/foo/bar::/data/foo.*", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "file_extension_2")]
    #[test_case(Input{source: "/foo/bar::/data/fo*.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}]; "file_substring_match")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "./"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}, Expectation{path: "bar.txt", data: "World"}]; "multi_file")]
    #[test_case(Input{source: "/foo/bar::/data/*fo*.txt", destination: "./"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/foobar.txt", data: "World"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}, Expectation{path: "foobar.txt", data: "World"}]; "multi_wildcard")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "./"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/foobar.txt", data: "World"},
                     File{ name: "foo.txt", data: "World"}, File{ name: "foobar.txt", data: "Hello"}]),
                vec![Expectation{path: "foo.txt", data: "Hello"}, Expectation{path: "foobar.txt", data: "World"}]; "multi_file_overwrite")]
    #[fuchsia::test]
    async fn copy_device_to_local_wildcard(
        input: Input,
        foo_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        let (realm_query, _, _) = create_realm_query(foo_files, vec![]);
        let destination_path = local_path.join(input.destination);

        copy_cmd(
            &realm_query,
            vec![input.source.to_string(), destination_path.display().to_string()],
            /*verbose=*/ true,
            std::io::stdout(),
        )
        .await
        .unwrap();

        for expected in expectation {
            let expected_data = expected.data.to_owned().into_bytes();
            let actual_data_path = local_path.join(expected.path);

            eprintln!("reading file '{}'", actual_data_path.display());

            let actual_data = read(actual_data_path).unwrap();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: "/wrong_moniker/foo/bar::/data/foo.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]); "bad_moniker")]
    #[test_case(Input{source: "/foo/bar::/data/bar.txt", destination: "foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]); "bad_file")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "bar/foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]); "bad_directory")]
    #[fuchsia::test]
    async fn copy_device_to_local_fails(input: Input, foo_files: Vec<SeedPath>) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        let (realm_query, _, _) = create_realm_query(foo_files, vec![]);
        let destination_path = local_path.join(input.destination).display().to_string();
        let result = copy_cmd(
            &realm_query,
            vec![input.source.to_string(), destination_path],
            /*verbose=*/ true,
            std::io::stdout(),
        )
        .await;

        assert!(result.is_err());
    }

    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/foo.txt"},
                generate_directory_paths(vec!["data"]),
                Expectation{path: "data/foo.txt", data: "Hello"}; "single_file")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/bar.txt"},
                generate_directory_paths(vec!["data"]),
                Expectation{path: "data/bar.txt", data: "Hello"}; "different_file_name")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "World"}]),
                Expectation{path: "data/foo.txt", data: "Hello"}; "overwrite_file")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "data/foo.txt", data: "Hello"}; "infer_path")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                Expectation{path: "data/foo.txt", data: "Hello"}; "infer_slash_path")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/nested/foo.txt"},
                generate_directory_paths(vec!["data", "data/nested"]),
                Expectation{path: "data/nested/foo.txt", data: "Hello"}; "nested_path")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/nested"},
                generate_directory_paths(vec!["data", "data/nested"]),
                Expectation{path: "data/nested/foo.txt", data: "Hello"}; "infer_nested_path")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/"},
                generate_directory_paths(vec!["data"]),
                Expectation{path: "data/foo.txt", data: LARGE_FILE_DATA}; "large_file")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar::/data/"},
                generate_directory_paths(vec!["data"]),
                Expectation{path: "data/foo.txt", data: OVER_LIMIT_FILE_DATA}; "over_channel_limit_file")]
    #[fuchsia::test]
    async fn copy_local_to_device(
        input: Input,
        foo_files: Vec<SeedPath>,
        expectation: Expectation,
    ) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        let source_path = local_path.join(&input.source);
        write(&source_path, expectation.data.to_owned().into_bytes()).unwrap();
        let (realm_query, foo_path, _) = create_realm_query(foo_files, vec![]);

        copy_cmd(
            &realm_query,
            vec![source_path.display().to_string(), input.destination.to_string()],
            /*verbose=*/ false,
            std::io::stdout(),
        )
        .await
        .unwrap();

        let actual_path = foo_path.join(expectation.path);
        let actual_data = read(actual_path).unwrap();
        let expected_data = expectation.data.to_owned().into_bytes();
        assert_eq!(actual_data, expected_data);
    }

    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "/bar/foo::/data/foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}]; "single_file")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "/bar/foo::/data/nested/foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data", "data/nested"]),
                vec![Expectation{path: "data/nested/foo.txt", data: "Hello"}]; "nested")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "/bar/foo::/data/bar.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/bar.txt", data: "Hello"}]; "different_file_name")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "/bar/foo::/data/foo.txt"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}]; "overwrite_file")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "/bar/foo::/data"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "wildcard_match_all_multi_file")]
    #[test_case(Input{source: "/foo/bar::/data/*.txt", destination: "/bar/foo::/data"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "wildcard_match_files_extensions_multi_file")]
    #[test_case(Input{source: "/foo/bar::/data/*", destination: "/bar/foo::/data"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "World"}, File{ name: "data/bar.txt", data: "Hello"}]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "wildcard_match_all_multi_file_overwrite")]
    #[fuchsia::test]
    async fn copy_device_to_device(
        input: Input,
        foo_files: Vec<SeedPath>,
        bar_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let (realm_query, _, bar_path) = create_realm_query(foo_files, bar_files);

        copy_cmd(
            &realm_query,
            vec![input.source.to_string(), input.destination.to_string()],
            /*verbose=*/ false,
            std::io::stdout(),
        )
        .await
        .unwrap();

        for expected in expectation {
            let destination_path = bar_path.join(expected.path);
            let actual_data = read(destination_path).unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: "/foo/bar::/data/cat.txt", destination: "/bar/foo::/data/foo.txt"}; "bad_file")]
    #[test_case(Input{source: "/foo/bar::/foo.txt", destination: "/bar/foo::/data/foo.txt"}; "bad_source_folder")]
    #[test_case(Input{source: "/hello/world::/data/foo.txt", destination: "/bar/foo::/data/file.txt"}; "bad_source_moniker")]
    #[test_case(Input{source: "/foo/bar::/data/foo.txt", destination: "/hello/world::/data/file.txt"}; "bad_destination_moniker")]
    #[fuchsia::test]
    async fn copy_device_to_device_fails(input: Input) {
        let (realm_query, _, _) = create_realm_query(
            generate_file_paths(vec![
                File { name: "data/foo.txt", data: "Hello" },
                File { name: "data/bar.txt", data: "World" },
            ]),
            generate_directory_paths(vec!["data"]),
        );

        let result = copy_cmd(
            &realm_query,
            vec![input.source.to_string(), input.destination.to_string()],
            /*verbose=*/ false,
            std::io::stdout(),
        )
        .await;

        assert!(result.is_err());
    }

    #[test_case(Inputs{sources: vec!["foo.txt"], destination: "/foo/bar::/data/"},
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}]; "single_file_wildcard")]
    #[test_case(Inputs{sources: vec!["foo.txt"], destination: "/foo/bar::/data/"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "World"}]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}]; "single_file_wildcard_overwrite")]
    #[test_case(Inputs{sources: vec!["foo.txt", "bar.txt"], destination: "/foo/bar::/data/"},
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "multi_file_wildcard")]
    #[test_case(Inputs{sources: vec!["foo.txt", "bar.txt"], destination: "/foo/bar::/data/"},
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "World"}, File{ name: "data/bar.txt", data: "World"}]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "multi_wildcard_file_overwrite")]
    #[fuchsia::test]
    async fn copy_local_to_device_wildcard(
        input: Inputs,
        foo_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        for (path, expected) in zip(input.sources.clone(), expectation.clone()) {
            let source_path = local_path.join(path);
            write(&source_path, expected.data).unwrap();
        }

        let (realm_query, foo_path, _) = create_realm_query(foo_files, vec![]);
        let mut paths: Vec<String> = input
            .sources
            .into_iter()
            .map(|path| local_path.join(path).display().to_string())
            .collect();
        paths.push(input.destination.to_string());

        copy_cmd(&realm_query, paths, /*verbose=*/ false, std::io::stdout()).await.unwrap();

        for expected in expectation {
            let actual_path = foo_path.join(expected.path);
            let actual_data = read(actual_path).unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }

    #[test_case(Input{source: "foo.txt", destination: "wrong_moniker/foo/bar::/data/foo.txt"}; "bad_moniker")]
    #[test_case(Input{source: "foo.txt", destination: "/foo/bar:://bar/foo.txt"}; "bad_directory")]
    #[fuchsia::test]
    async fn copy_local_to_device_fails(input: Input) {
        let local_dir = create_tmp_dir(vec![]).unwrap();
        let local_path = local_dir.path();

        let source_path = local_path.join(input.source);
        write(&source_path, "Hello".to_owned().into_bytes()).unwrap();

        let (realm_query, _, _) =
            create_realm_query(generate_directory_paths(vec!["data"]), vec![]);

        let result = copy_cmd(
            &realm_query,
            vec![source_path.display().to_string(), input.destination.to_string()],
            /*verbose=*/ false,
            std::io::stdout(),
        )
        .await;

        assert!(result.is_err());
    }

    #[test_case(vec![]; "no_wildcard_matches")]
    #[test_case(vec!["foo.txt"]; "not_enough_args")]
    #[test_case(vec!["/foo/bar::/data/*", "/foo/bar::/data/*"]; "remote_wildcard_destination")]
    #[test_case(vec!["/foo/bar::/data/*", "/foo/bar::/data/*", "/"]; "multi_wildcards_remote")]
    #[test_case(vec!["*", "*"]; "local_wildcard_destination")]
    #[fuchsia::test]
    async fn copy_wildcard_fails(paths: Vec<&str>) {
        let (realm_query, _, _) = create_realm_query(
            generate_file_paths(vec![File { name: "data/foo.txt", data: "Hello" }]),
            vec![],
        );
        let paths = paths.into_iter().map(|s| s.to_string()).collect();
        let result = copy_cmd(&realm_query, paths, /*verbose=*/ false, std::io::stdout()).await;

        assert!(result.is_err());
    }

    #[test_case(Inputs{sources: vec!["/foo/bar::/data/foo.txt", "bar.txt"], destination: "/bar/foo::/data/"},
                generate_file_paths(vec![File{ name: "bar.txt", data: "World"}]),
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}]; "no_wildcard_mix")]
    #[test_case(Inputs{sources: vec!["/foo/bar::/data/foo.txt", "/foo/bar::/data/*", "foobar.txt"], destination: "/bar/foo::/data/"},
                generate_file_paths(vec![File{ name: "foobar.txt", data: "World"}]),
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}, Expectation{path: "data/foobar.txt", data: "World"}]; "wildcard_mix")]
    #[test_case(Inputs{sources: vec!["/foo/bar::/data/*", "/foo/bar::/data/*", "foobar.txt"], destination: "/bar/foo::/data/"},
                generate_file_paths(vec![File{ name: "foobar.txt", data: "World"}]),
                generate_file_paths(vec![File{ name: "data/foo.txt", data: "Hello"}, File{ name: "data/bar.txt", data: "World"}]),
                generate_directory_paths(vec!["data"]),
                vec![Expectation{path: "data/foo.txt", data: "Hello"}, Expectation{path: "data/bar.txt", data: "World"}, Expectation{path: "data/foobar.txt", data: "World"}]; "double_wildcard")]
    #[fuchsia::test]
    async fn copy_mixed_tests_remote_destination(
        input: Inputs,
        local_files: Vec<SeedPath>,
        foo_files: Vec<SeedPath>,
        bar_files: Vec<SeedPath>,
        expectation: Vec<Expectation>,
    ) {
        let local_dir = create_tmp_dir(local_files).unwrap();
        let local_path = local_dir.path();

        let (realm_query, _, bar_path) = create_realm_query(foo_files, bar_files);
        let mut paths: Vec<String> = input
            .sources
            .clone()
            .into_iter()
            .map(|path| match LocalOrRemotePath::parse(&path) {
                LocalOrRemotePath::Remote(_) => path.to_string(),
                LocalOrRemotePath::Local(_) => local_path.join(path).display().to_string(),
            })
            .collect();
        paths.push(input.destination.to_owned());

        copy_cmd(&realm_query, paths, /*verbose=*/ false, std::io::stdout()).await.unwrap();

        for expected in expectation {
            let actual_path = bar_path.join(expected.path);
            let actual_data = read(actual_path).unwrap();
            let expected_data = expected.data.to_owned().into_bytes();
            assert_eq!(actual_data, expected_data);
        }
    }
}
