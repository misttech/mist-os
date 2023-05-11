// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{dirs_to_test, repeat_by_n, PackageSource},
    anyhow::{anyhow, Context as _, Error},
    assert_matches::assert_matches,
    fidl::{
        endpoints::{create_proxy, Proxy as _},
        AsHandleRef as _,
    },
    fidl_fuchsia_io as fio,
    fuchsia_fs::directory::open_directory,
    fuchsia_fs::directory::{DirEntry, DirentKind},
    fuchsia_zircon as zx,
    futures::{future::Future, StreamExt},
    itertools::Itertools as _,
    pretty_assertions::assert_eq,
    std::{
        clone::Clone,
        collections::HashSet,
        convert::TryInto,
        iter::{FromIterator, IntoIterator},
    },
};

#[fuchsia::test]
async fn open() {
    for source in dirs_to_test().await {
        open_per_package_source(source).await
    }
}

async fn open_per_package_source(source: PackageSource) {
    // Testing dimensions:
    //   1. Receiver of the open call: /, meta/, subdir below meta/, subdir not below meta/
    //   2. Type of node the path points at: self, meta/, subdir below meta/, file below meta/,
    //      subdir not below meta/, file not below meta/ (not all receivers can open every type of
    //      target)
    //   3. Whether the path being opened is segmented
    // The flags and modes are handled by the helper functions.
    assert_open_root_directory(&source, ".", ".").await;
    assert_open_content_directory(&source, ".", "dir").await;
    assert_open_content_directory(&source, ".", "dir/dir").await;
    assert_open_content_file(&source, ".", "file").await;
    assert_open_content_file(&source, ".", "dir/file").await;
    assert_open_meta_as_directory_and_file(&source, ".", "meta").await;
    assert_open_meta_subdirectory(&source, ".", "meta/dir").await;
    assert_open_meta_file(&source, ".", "meta/file").await;

    // Self-opening "meta" does not trigger the file/dir duality.
    assert_open_meta_subdirectory(&source, "meta", ".").await;
    assert_open_meta_subdirectory(&source, "meta", "dir").await;
    assert_open_meta_subdirectory(&source, "meta", "dir/dir").await;
    assert_open_meta_file(&source, "meta", "file").await;
    assert_open_meta_file(&source, "meta", "dir/file").await;

    assert_open_meta_subdirectory(&source, "meta/dir", ".").await;
    assert_open_meta_subdirectory(&source, "meta/dir", "dir").await;
    assert_open_meta_subdirectory(&source, "meta/dir", "dir/dir").await;
    assert_open_meta_file(&source, "meta/dir", "file").await;
    assert_open_meta_file(&source, "meta/dir", "dir/file").await;

    assert_open_content_directory(&source, "dir", ".").await;
    assert_open_content_directory(&source, "dir", "dir").await;
    assert_open_content_directory(&source, "dir", "dir/dir").await;
    assert_open_content_file(&source, "dir", "file").await;
    assert_open_content_file(&source, "dir", "dir/file").await;
}

const ALL_FLAGS: [fio::OpenFlags; 15] = [
    fio::OpenFlags::empty(),
    fio::OpenFlags::RIGHT_READABLE,
    fio::OpenFlags::RIGHT_WRITABLE,
    fio::OpenFlags::RIGHT_EXECUTABLE,
    fio::OpenFlags::CREATE,
    fio::OpenFlags::empty().union(fio::OpenFlags::CREATE).union(fio::OpenFlags::CREATE_IF_ABSENT),
    fio::OpenFlags::CREATE_IF_ABSENT,
    fio::OpenFlags::TRUNCATE,
    fio::OpenFlags::DIRECTORY,
    fio::OpenFlags::APPEND,
    fio::OpenFlags::NODE_REFERENCE,
    fio::OpenFlags::DESCRIBE,
    fio::OpenFlags::POSIX_WRITABLE,
    fio::OpenFlags::POSIX_EXECUTABLE,
    fio::OpenFlags::NOT_DIRECTORY,
];

async fn assert_open_root_directory(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let package_root = &source.dir;

    let success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::RIGHT_EXECUTABLE,
        fio::OpenFlags::DIRECTORY,
        fio::OpenFlags::NODE_REFERENCE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
    ];

    let child_paths = generate_valid_directory_paths(child_base_path);
    let lax_child_paths = generate_lax_directory_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let success_flags_and_child_paths =
        itertools::iproduct!(success_flags, child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        success_flags_and_child_paths.clone(),
        verify_directory_opened,
    )
    .await;

    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        subtract(all_flag_and_child_paths, success_flags_and_child_paths).into_iter(),
        verify_open_failed,
    )
    .await;
}

fn filter_out_contradictory_open_parameters(
    (flag, child_path): (fio::OpenFlags, &str),
) -> Option<(fio::OpenFlags, &'_ str)> {
    if flag.intersects(fio::OpenFlags::NOT_DIRECTORY) && child_path.ends_with('/') {
        None
    } else {
        Some((flag, child_path))
    }
}

async fn assert_open_success<V, Fut>(
    package_root: &fio::DirectoryProxy,
    parent_path: &str,
    allowed_flags_and_child_paths: impl Iterator<Item = (fio::OpenFlags, &str)>,
    verifier: V,
) where
    V: Fn(fio::NodeProxy, fio::OpenFlags) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let parent = open_parent(package_root, parent_path).await;
    for (flag, child_path) in allowed_flags_and_child_paths {
        let node = open_node(&parent, flag, child_path);
        if let Err(e) = verifier(node, flag).await {
            panic!(
                "failed to verify open. parent: {parent_path:?}, child: {child_path:?}, flag: {flag:?}, \
                       error: {e:#}"
            );
        }
    }
}

async fn assert_open_content_directory(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let package_root = &source.dir;

    let success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::RIGHT_EXECUTABLE,
        fio::OpenFlags::DIRECTORY,
        fio::OpenFlags::NODE_REFERENCE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
    ];
    let child_paths = generate_valid_directory_paths(child_base_path);
    let lax_child_paths = generate_lax_directory_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let success_flags_and_child_paths =
        itertools::iproduct!(success_flags, child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        success_flags_and_child_paths.clone(),
        verify_directory_opened,
    )
    .await;

    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        subtract(all_flag_and_child_paths, success_flags_and_child_paths).into_iter(),
        verify_open_failed,
    )
    .await;
}

fn subtract<'a, I, J, T>(minuend: I, subtrahend: J) -> Vec<T>
where
    I: IntoIterator<Item = T>,
    <I as IntoIterator>::IntoIter: Clone + 'a,
    J: IntoIterator<Item = T>,
    T: Eq + std::hash::Hash + 'a,
{
    let subtrahend = HashSet::<T>::from_iter(subtrahend);
    minuend.into_iter().filter(|v| !subtrahend.contains(v)).collect()
}

#[test]
fn test_subtract() {
    assert_eq!(subtract(["foo", "bar"], ["bar", "baz"]), vec!["foo"]);
}

async fn assert_open_flag_and_child_path_failure<V, Fut>(
    package_root: &fio::DirectoryProxy,
    parent_path: &str,
    disallowed_flags_and_child_paths: impl Iterator<Item = (fio::OpenFlags, &str)>,
    verifier: V,
) where
    V: Fn(fio::NodeProxy) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let parent = open_parent(package_root, parent_path).await;
    for (flag, child_path) in disallowed_flags_and_child_paths {
        let node = open_node(&parent, flag, child_path);
        if let Err(e) = verifier(node).await {
            panic!(
                "failed to verify open failed. parent: {parent_path:?}, child: {child_path:?}, flag: {flag:?}, \
                       error: {e:#}"
            );
        }
    }
}

async fn assert_open_content_file(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let package_root = &source.dir;

    let success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::RIGHT_EXECUTABLE,
        fio::OpenFlags::APPEND,
        fio::OpenFlags::NODE_REFERENCE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
        fio::OpenFlags::NOT_DIRECTORY,
    ];

    let child_paths = generate_valid_file_paths(child_base_path);
    let lax_child_paths = generate_lax_directory_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let success_flags_and_child_paths =
        itertools::iproduct!(success_flags, child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        success_flags_and_child_paths.clone(),
        verify_content_file_opened,
    )
    .await;

    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        subtract(all_flag_and_child_paths, success_flags_and_child_paths).into_iter(),
        verify_open_failed,
    )
    .await;
}

async fn assert_open_meta_as_directory_and_file(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let package_root = &source.dir;

    let directory_success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
    ];

    // To open "meta" as a directory at least one of the following must be true:
    //   1. OPEN_FLAG_DIRECTORY is set
    //   2. OPEN_FLAG_NODE_REFERENCE is set
    let directory_flags = std::iter::empty()
        .chain(directory_success_flags.iter().copied().map(|f| f | fio::OpenFlags::DIRECTORY))
        .chain(directory_success_flags.iter().copied().map(|f| f | fio::OpenFlags::NODE_REFERENCE));

    let directory_child_paths = generate_valid_directory_paths(child_base_path);
    let lax_child_paths = generate_lax_directory_paths(child_base_path);

    let directory_only_child_paths = generate_valid_directory_only_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let directory_flags_and_child_paths =
        itertools::iproduct!(directory_flags, directory_child_paths.iter().map(String::as_str))
            .chain(itertools::iproduct!(
                directory_success_flags,
                directory_only_child_paths.iter().map(String::as_str)
            ))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        directory_flags_and_child_paths.clone(),
        verify_directory_opened,
    )
    .await;

    // To open "meta" as a file none of the following are true:
    //   1. OPEN_FLAG_DIRECTORY is set
    //   2. OPEN_FLAG_NODE_REFERENCE is set
    let file_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
        fio::OpenFlags::NOT_DIRECTORY,
    ];

    let file_child_paths = generate_valid_file_paths(child_base_path);

    let file_flags_and_child_paths =
        itertools::iproduct!(file_flags, file_child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);

    assert_open_success(
        package_root,
        parent_path,
        file_flags_and_child_paths.clone(),
        verify_meta_as_file_opened,
    )
    .await;

    let failure_flags_and_child_paths = subtract(
        subtract(all_flag_and_child_paths, directory_flags_and_child_paths),
        file_flags_and_child_paths,
    )
    .into_iter();
    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        failure_flags_and_child_paths,
        verify_open_failed,
    )
    .await;
}

async fn assert_open_meta_subdirectory(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let package_root = &source.dir;

    let success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::DIRECTORY,
        fio::OpenFlags::NODE_REFERENCE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
    ];

    let child_paths = generate_valid_directory_paths(child_base_path);

    let lax_child_paths = generate_lax_directory_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let success_flags_and_child_paths =
        itertools::iproduct!(success_flags, child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        success_flags_and_child_paths.clone(),
        verify_directory_opened,
    )
    .await;

    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        subtract(all_flag_and_child_paths, success_flags_and_child_paths).into_iter(),
        verify_open_failed,
    )
    .await;
}

async fn assert_open_meta_file(source: &PackageSource, parent_path: &str, child_base_path: &str) {
    let package_root = &source.dir;

    let success_flags = [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::NODE_REFERENCE,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::POSIX_WRITABLE,
        fio::OpenFlags::POSIX_EXECUTABLE,
        fio::OpenFlags::NOT_DIRECTORY,
    ];

    let child_paths = generate_valid_file_paths(child_base_path);

    let lax_child_paths = generate_lax_directory_paths(child_base_path);
    let all_flag_and_child_paths =
        itertools::iproduct!(ALL_FLAGS, lax_child_paths.iter().map(String::as_str));

    let success_flags_and_child_paths =
        itertools::iproduct!(success_flags, child_paths.iter().map(String::as_str))
            .filter_map(filter_out_contradictory_open_parameters);
    assert_open_success(
        package_root,
        parent_path,
        success_flags_and_child_paths.clone(),
        verify_meta_as_file_opened,
    )
    .await;

    assert_open_flag_and_child_path_failure(
        package_root,
        parent_path,
        subtract(all_flag_and_child_paths, success_flags_and_child_paths).into_iter(),
        verify_open_failed,
    )
    .await;
}

async fn open_parent(package_root: &fio::DirectoryProxy, parent_path: &str) -> fio::DirectoryProxy {
    let parent_rights = if parent_path == "meta"
        || parent_path == "/meta"
        || parent_path.starts_with("meta/")
        || parent_path.starts_with("/meta/")
    {
        fio::OpenFlags::RIGHT_READABLE
    } else {
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE
    };
    fuchsia_fs::directory::open_directory(package_root, parent_path, parent_rights)
        .await
        .expect("open parent directory")
}

fn open_node(parent: &fio::DirectoryProxy, flags: fio::OpenFlags, path: &str) -> fio::NodeProxy {
    let (node, server_end) = create_proxy::<fio::NodeMarker>().expect("create_proxy");
    parent.open(flags, fio::ModeType::empty(), path, server_end).expect("open node");
    node
}

/// Generates the same path variations as [`generate_valid_directory_paths`]
/// plus extra invalid path variations using segments of "." and "..", leading "/", trailing "/",
/// and repeated "/".
fn generate_lax_directory_paths(base: &str) -> Vec<String> {
    let mut paths = generate_valid_directory_paths(base);
    if base == "." {
        paths.extend([format!("{base}/"), format!("/{base}"), format!("/{base}/")]);
    }
    // "path segment rules are checked"
    paths.extend([format!("./{base}"), format!("{base}/.")]);
    if base.contains('/') {
        paths.push(base.replace('/', "//"));
        paths.push(base.replace('/', "/to-be-removed/../"));
        paths.push(base.replace('/', "/./"));
    }
    paths
}

/// Generates a set of path variations which are valid when opening directories.
fn generate_valid_directory_paths(base: &str) -> Vec<String> {
    if base == "." {
        vec![base.to_string()]
    } else {
        vec![base.to_string(), format!("{base}/"), format!("/{base}"), format!("/{base}/")]
    }
}

/// Generates a set of path variations which are only valid when opening directories.
///
/// Paths ending in "/" can only be used when opening directories.
fn generate_valid_directory_only_paths(base: &str) -> Vec<String> {
    if base == "." {
        return vec![];
    }
    vec![format!("{base}/"), format!("/{base}/")]
}

/// Generates a set of path variations which are valid when opening files.
fn generate_valid_file_paths(base: &str) -> Vec<String> {
    vec![base.to_string(), format!("/{base}")]
}

async fn verify_directory_opened(node: fio::NodeProxy, flag: fio::OpenFlags) -> Result<(), Error> {
    let protocol = node.query().await.context("failed to call describe")?;
    let expected = if flag.intersects(fio::OpenFlags::NODE_REFERENCE) {
        fio::NODE_PROTOCOL_NAME
    } else {
        fio::DIRECTORY_PROTOCOL_NAME
    };
    if protocol != expected.as_bytes() {
        return Err(anyhow!("wrong protocol returned: {:?}", std::str::from_utf8(&protocol)));
    }

    if flag.intersects(fio::OpenFlags::DESCRIBE) {
        let event = node.take_event_stream().next().await.ok_or_else(|| anyhow!("no events!"))?;
        let event = event.context("event error")?;
        match event {
            fio::NodeEvent::OnOpen_ { s, info } => {
                let () = zx::Status::ok(s).context("OnOpen failed")?;
                let info = info.ok_or_else(|| anyhow!("missing info"))?;
                let expected = if flag.intersects(fio::OpenFlags::NODE_REFERENCE) {
                    fio::NodeInfoDeprecated::Service(fio::Service)
                } else {
                    fio::NodeInfoDeprecated::Directory(fio::DirectoryObject)
                };
                if *info != expected {
                    return Err(anyhow!("wrong protocol returned: {:?}", info));
                }
            }
            event @ fio::NodeEvent::OnRepresentation { .. } => {
                return Err(anyhow!("unexpected event returned: {:?}", event));
            }
        }
    };
    Ok(())
}

async fn verify_content_file_opened(
    node: fio::NodeProxy,
    flag: fio::OpenFlags,
) -> Result<(), Error> {
    let protocol = node.query().await.context("failed to call query")?;
    if flag.intersects(fio::OpenFlags::NODE_REFERENCE) {
        if protocol != fio::NODE_PROTOCOL_NAME.as_bytes() {
            return Err(anyhow!("wrong protocol returned: {:?}", std::str::from_utf8(&protocol)));
        }
        if flag.intersects(fio::OpenFlags::DESCRIBE) {
            let event =
                node.take_event_stream().next().await.ok_or_else(|| anyhow!("no events!"))?;
            let event = event.context("event error")?;
            match event {
                fio::NodeEvent::OnOpen_ { s, info } => {
                    let () = zx::Status::ok(s).context("OnOpen failed")?;
                    let info = info.ok_or_else(|| anyhow!("missing info"))?;
                    if *info != fio::NodeInfoDeprecated::Service(fio::Service) {
                        return Err(anyhow!("wrong protocol returned: {:?}", info));
                    }
                }
                event @ fio::NodeEvent::OnRepresentation { .. } => {
                    return Err(anyhow!("unexpected event returned: {:?}", event));
                }
            }
        }
    } else {
        if protocol != fio::FILE_PROTOCOL_NAME.as_bytes() {
            return Err(anyhow!("wrong protocol returned: {:?}", std::str::from_utf8(&protocol)));
        }
        let file = fio::FileProxy::new(node.into_channel().unwrap());
        {
            let fio::FileInfo { observer, .. } =
                file.describe().await.context("failed to call describe")?;
            let observer = observer.ok_or_else(|| anyhow!("expected observer to be set"))?;
            let _: zx::Signals = observer
                .wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST)
                .context("FILE_SIGNAL_READABLE not set")?;
        }

        if flag.intersects(fio::OpenFlags::NODE_REFERENCE) {
            let event =
                file.take_event_stream().next().await.ok_or_else(|| anyhow!("no events!"))?;
            let event = event.context("event error")?;
            match event {
                fio::FileEvent::OnOpen_ { s, info } => {
                    let () = zx::Status::ok(s).context("OnOpen failed")?;
                    let info = info.ok_or_else(|| anyhow!("missing info"))?;
                    if let fio::NodeInfoDeprecated::File(fio::FileObject { event, stream: _ }) =
                        *info
                    {
                        let event = event.ok_or_else(|| anyhow!("expected event to be set"))?;
                        let _: zx::Signals = event
                            .wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST)
                            .context("FILE_SIGNAL_READABLE not set")?;
                    } else {
                        return Err(anyhow!("wrong protocol returned: {:?}", info));
                    }
                }
                event @ fio::FileEvent::OnRepresentation { .. } => {
                    return Err(anyhow!("unexpected event returned: {:?}", event));
                }
            }
        }
    }
    Ok(())
}

async fn verify_meta_as_file_opened(
    node: fio::NodeProxy,
    flag: fio::OpenFlags,
) -> Result<(), Error> {
    let protocol = node.query().await.context("failed to call describe")?;
    let expected = if flag.intersects(fio::OpenFlags::NODE_REFERENCE) {
        fio::NODE_PROTOCOL_NAME
    } else {
        fio::FILE_PROTOCOL_NAME
    };
    if protocol != expected.as_bytes() {
        return Err(anyhow!("wrong protocol returned: {:?}", std::str::from_utf8(&protocol)));
    }

    if flag.intersects(fio::OpenFlags::DESCRIBE) {
        let event = node.take_event_stream().next().await.ok_or_else(|| anyhow!("no events!"))?;
        let event = event.context("event error")?;
        match event {
            fio::NodeEvent::OnOpen_ { s, info } => {
                let () = zx::Status::ok(s).context("OnOpen failed")?;
                let info = info.ok_or_else(|| anyhow!("missing info"))?;
                match *info {
                    fio::NodeInfoDeprecated::File(fio::FileObject { .. }) => {}
                    info => return Err(anyhow!("wrong protocol returned: {:?}", info)),
                }
            }
            event @ fio::NodeEvent::OnRepresentation { .. } => {
                return Err(anyhow!("unexpected event returned: {:?}", event));
            }
        }
    }
    Ok(())
}

async fn verify_open_failed(node: fio::NodeProxy) -> Result<(), Error> {
    match node.query().await {
        Ok(protocol) => Err(anyhow!("node should be closed: {:?}", protocol)),
        Err(fidl::Error::ClientChannelClosed { status: _, protocol_name: _ }) => Ok(()),
        Err(e) => Err(e).context("failed with unexpected error"),
    }
}

#[fuchsia::test]
async fn clone() {
    for source in dirs_to_test().await {
        clone_per_package_source(source).await
    }
}

async fn clone_per_package_source(source: PackageSource) {
    let root_dir = &source.dir;

    assert_clone_sends_on_open_event(root_dir, ".").await;
    assert_clone_sends_on_open_event(root_dir, "dir").await;
    assert_clone_sends_on_open_event(root_dir, "meta").await;
    assert_clone_sends_on_open_event(root_dir, "meta/dir").await;

    for flag in [
        fio::OpenFlags::empty(),
        fio::OpenFlags::RIGHT_READABLE,
        fio::OpenFlags::RIGHT_WRITABLE,
        fio::OpenFlags::RIGHT_EXECUTABLE,
        fio::OpenFlags::APPEND,
        fio::OpenFlags::DESCRIBE,
        fio::OpenFlags::CLONE_SAME_RIGHTS,
    ] {
        if flag.intersects(fio::OpenFlags::APPEND) {
            continue;
        }
        if flag.intersects(fio::OpenFlags::RIGHT_WRITABLE) {
            continue;
        }

        assert_clone_directory_overflow(
            root_dir,
            ".",
            flag,
            vec![
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry {
                    name: "dir_overflow_readdirents".to_string(),
                    kind: DirentKind::Directory,
                },
                DirEntry { name: "exceeds_max_buf".to_string(), kind: DirentKind::File },
                DirEntry { name: "file".to_string(), kind: DirentKind::File },
                DirEntry { name: "meta".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "file_0".to_string(), kind: DirentKind::File },
                DirEntry { name: "file_1".to_string(), kind: DirentKind::File },
                DirEntry { name: "file_4095".to_string(), kind: DirentKind::File },
                DirEntry { name: "file_4096".to_string(), kind: DirentKind::File },
                DirEntry { name: "file_4097".to_string(), kind: DirentKind::File },
            ],
        )
        .await;

        assert_clone_directory_no_overflow(
            root_dir,
            "dir",
            flag,
            vec![
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "file".to_string(), kind: DirentKind::File },
            ],
        )
        .await;
        if flag.intersects(fio::OpenFlags::RIGHT_EXECUTABLE) {
            // neither the "meta" dir nor meta subdirectories can be opened with the executable
            // right, so they can not be cloned with the executable right.
        } else {
            assert_clone_directory_overflow(
                root_dir,
                "meta",
                flag,
                vec![
                    DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry {
                        name: "dir_overflow_readdirents".to_string(),
                        kind: DirentKind::Directory,
                    },
                    DirEntry { name: "exceeds_max_buf".to_string(), kind: DirentKind::File },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                    DirEntry { name: "package".to_string(), kind: DirentKind::File },
                    DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file_0".to_string(), kind: DirentKind::File },
                    DirEntry { name: "file_1".to_string(), kind: DirentKind::File },
                    DirEntry { name: "file_4095".to_string(), kind: DirentKind::File },
                    DirEntry { name: "file_4096".to_string(), kind: DirentKind::File },
                    DirEntry { name: "file_4097".to_string(), kind: DirentKind::File },
                ],
            )
            .await;
            assert_clone_directory_no_overflow(
                root_dir,
                "meta/dir",
                flag,
                vec![
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                ],
            )
            .await;
        }
    }
}

async fn assert_clone_sends_on_open_event(package_root: &fio::DirectoryProxy, path: &str) {
    async fn verify_directory_clone_sends_on_open_event(node: fio::NodeProxy) -> Result<(), Error> {
        match node.take_event_stream().next().await {
            Some(Ok(fio::NodeEvent::OnOpen_ { s, info: Some(boxed) })) => {
                assert_eq!(zx::Status::from_raw(s), zx::Status::OK);
                assert_eq!(*boxed, fio::NodeInfoDeprecated::Directory(fio::DirectoryObject {}));
                Ok(())
            }
            Some(Ok(other)) => Err(anyhow!("wrong node event returned: {:?}", other)),
            Some(Err(e)) => Err(e).context("failed to call onopen"),
            None => Err(anyhow!("no events!")),
        }
    }

    let parent = open_parent(package_root, path).await;
    let (node, server_end) = create_proxy::<fio::NodeMarker>().expect("create_proxy");
    parent.clone(fio::OpenFlags::DESCRIBE, server_end).expect("clone dir");
    if let Err(e) = verify_directory_clone_sends_on_open_event(node).await {
        panic!("failed to verify clone. path: {path:?}, error: {e:#}");
    }
}

async fn assert_clone_directory_no_overflow(
    package_root: &fio::DirectoryProxy,
    path: &str,
    flags: fio::OpenFlags,
    expected_dirents: Vec<DirEntry>,
) {
    let parent = open_directory(
        package_root,
        path,
        flags & (fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE),
    )
    .await
    .expect("open parent directory");
    let (clone, server_end) = create_proxy::<fio::DirectoryMarker>().expect("create_proxy");

    let node_request = fidl::endpoints::ServerEnd::new(server_end.into_channel());
    parent.clone(flags, node_request).expect("cloned node");

    assert_read_dirents_no_overflow(&clone, expected_dirents).await;
}

async fn assert_clone_directory_overflow(
    package_root: &fio::DirectoryProxy,
    path: &str,
    flags: fio::OpenFlags,
    expected_dirents: Vec<DirEntry>,
) {
    let parent = open_parent(package_root, path).await;
    let (clone, server_end) = create_proxy::<fio::DirectoryMarker>().expect("create_proxy");

    let node_request = fidl::endpoints::ServerEnd::new(server_end.into_channel());
    parent.clone(flags, node_request).expect("cloned node");

    assert_read_dirents_overflow(&clone, expected_dirents).await;
}

#[fuchsia::test]
async fn read_dirents() {
    for source in dirs_to_test().await {
        read_dirents_per_package_source(source).await
    }
}

async fn read_dirents_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    // Handle overflow cases (e.g. when size of total dirents exceeds MAX_BUF).
    assert_read_dirents_overflow(
        &root_dir,
        vec![
            DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "dir_overflow_readdirents".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "exceeds_max_buf".to_string(), kind: DirentKind::File },
            DirEntry { name: "file".to_string(), kind: DirentKind::File },
            DirEntry { name: "meta".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "file_0".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_1".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4095".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4096".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4097".to_string(), kind: DirentKind::File },
        ],
    )
    .await;
    assert_read_dirents_overflow(
        &fuchsia_fs::directory::open_directory(&root_dir, "meta", fio::OpenFlags::empty())
            .await
            .expect("open meta as dir"),
        vec![
            DirEntry { name: "contents".to_string(), kind: DirentKind::File },
            DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "dir_overflow_readdirents".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "exceeds_max_buf".to_string(), kind: DirentKind::File },
            DirEntry { name: "file".to_string(), kind: DirentKind::File },
            DirEntry { name: "package".to_string(), kind: DirentKind::File },
            DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "file_0".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_1".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4095".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4096".to_string(), kind: DirentKind::File },
            DirEntry { name: "file_4097".to_string(), kind: DirentKind::File },
        ],
    )
    .await;
    assert_read_dirents_overflow(
        &fuchsia_fs::directory::open_directory(
            &root_dir,
            "dir_overflow_readdirents",
            fio::OpenFlags::empty(),
        )
        .await
        .expect("open dir_overflow_readdirents"),
        vec![],
    )
    .await;
    assert_read_dirents_overflow(
        &fuchsia_fs::directory::open_directory(
            &root_dir,
            "meta/dir_overflow_readdirents",
            fio::OpenFlags::empty(),
        )
        .await
        .expect("open meta/dir_overflow_readdirents"),
        vec![],
    )
    .await;

    // Handle no-overflow cases (e.g. when size of total dirents does not exceed MAX_BUF).
    assert_read_dirents_no_overflow(
        &fuchsia_fs::directory::open_directory(&root_dir, "dir", fio::OpenFlags::empty())
            .await
            .expect("open dir"),
        vec![
            DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "file".to_string(), kind: DirentKind::File },
        ],
    )
    .await;
    assert_read_dirents_no_overflow(
        &fuchsia_fs::directory::open_directory(&root_dir, "meta/dir", fio::OpenFlags::empty())
            .await
            .expect("open meta/dir"),
        vec![
            DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
            DirEntry { name: "file".to_string(), kind: DirentKind::File },
        ],
    )
    .await;
}

/// For a particular directory, verify that the overflow case is being hit on ReadDirents (e.g. it
/// should take two ReadDirents calls to read all of the directory entries).
/// Note: we considered making this a unit test for pkg-harness, but opted to include this in the
/// integration tests so all the test cases are in one place.
async fn assert_read_dirents_overflow(
    dir: &fio::DirectoryProxy,
    additional_contents: Vec<DirEntry>,
) {
    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "first call should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "second call should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert_eq!(buf, []);

    assert_eq!(
        fuchsia_fs::directory::readdir(dir).await.unwrap().into_iter().sorted().collect::<Vec<_>>(),
        ('a'..='z')
            .chain('A'..='E')
            .map(|seed| DirEntry {
                name: repeat_by_n(seed, fio::MAX_FILENAME.try_into().unwrap()),
                kind: DirentKind::File
            })
            .chain(additional_contents)
            .sorted()
            .collect::<Vec<_>>()
    );
}

/// For a particular directory, verify that the overflow case is NOT being hit on ReadDirents
/// (e.g. it should only take one ReadDirents call to read all of the directory entries).
async fn assert_read_dirents_no_overflow(
    dir: &fio::DirectoryProxy,
    expected_dirents: Vec<DirEntry>,
) {
    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "first call should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert_eq!(buf, []);

    assert_eq!(
        fuchsia_fs::directory::readdir(dir).await.unwrap().into_iter().sorted().collect::<Vec<_>>(),
        expected_dirents.into_iter().sorted().collect::<Vec<_>>()
    );
}

#[fuchsia::test]
async fn rewind() {
    for source in dirs_to_test().await {
        rewind_per_package_source(source).await
    }
}

async fn rewind_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    // Handle overflow cases.
    for path in [".", "meta", "dir_overflow_readdirents", "meta/dir_overflow_readdirents"] {
        let dir = fuchsia_fs::directory::open_directory(&root_dir, path, fio::OpenFlags::empty())
            .await
            .unwrap();
        assert_rewind_overflow_when_seek_offset_at_end(&dir).await;
        assert_rewind_overflow_when_seek_offset_in_middle(&dir).await;
    }

    // Handle non-overflow cases.
    for path in ["dir", "meta/dir"] {
        assert_rewind_no_overflow(
            &fuchsia_fs::directory::open_directory(&root_dir, path, fio::OpenFlags::empty())
                .await
                .unwrap(),
        )
        .await;
    }
}

async fn assert_rewind_overflow_when_seek_offset_at_end(dir: &fio::DirectoryProxy) {
    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "first read_dirents call should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "second read_dirents call should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert_eq!(buf, []);

    let status = dir.rewind().await.unwrap();
    zx::Status::ok(status).expect("status ok");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "read_dirents call after rewind should yield non-empty buffer");
}

async fn assert_rewind_overflow_when_seek_offset_in_middle(dir: &fio::DirectoryProxy) {
    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "first read_dirents call should yield non-empty buffer");

    let status = dir.rewind().await.unwrap();
    zx::Status::ok(status).expect("status ok");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "first read_dirents call after rewind should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf.is_empty(), "second read_dirents call after rewind should yield non-empty buffer");

    let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert_eq!(buf, []);
}

async fn assert_rewind_no_overflow(dir: &fio::DirectoryProxy) {
    let (status, buf0) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf0.is_empty(), "first read_dirents call should yield non-empty buffer");

    let status = dir.rewind().await.unwrap();
    zx::Status::ok(status).expect("status ok");

    let (status, buf1) = dir.read_dirents(fio::MAX_BUF).await.unwrap();
    zx::Status::ok(status).expect("status ok");
    assert!(!buf1.is_empty(), "first read_dirents call after rewind should yield non-empty buffer");

    // We can't guarantee ordering will be the same, so the next best thing is to verify the
    // returned buffers are the same length.
    assert_eq!(buf0.len(), buf1.len());
}

#[fuchsia::test]
async fn get_token() {
    for source in dirs_to_test().await {
        get_token_per_package_source(source).await
    }
}

async fn get_token_per_package_source(source: PackageSource) {
    let root_dir = &source.dir;
    for path in [".", "dir", "meta", "meta/dir"] {
        let dir = fuchsia_fs::directory::open_directory(root_dir, path, fio::OpenFlags::empty())
            .await
            .unwrap();

        let (status, token) = dir.get_token().await.unwrap();
        let status = zx::Status::ok(status);
        assert_eq!(status, Err(zx::Status::NOT_SUPPORTED));
        assert!(token.is_none(), "token should be absent");
    }
}

#[fuchsia::test]
async fn get_flags() {
    for source in dirs_to_test().await {
        get_flags_per_package_source(source).await
    }
}

async fn get_flags_per_package_source(source: PackageSource) {
    // Test get_flags APIs for root directory and subdirectory.
    assert_get_flags_directory_calls(
        &source,
        ".",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .await;
    assert_get_flags_directory_calls(
        &source,
        "dir",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .await;

    // Test get_flags APIs for meta directory and subdirectory.
    assert_get_flags_directory_calls(&source, "meta", fio::OpenFlags::RIGHT_READABLE).await;
    assert_get_flags_directory_calls(&source, "meta/dir", fio::OpenFlags::RIGHT_READABLE).await;
}

async fn assert_get_flags_directory_calls(
    source: &PackageSource,
    path: &str,
    expected_rights: fio::OpenFlags,
) {
    let package_root = &source.dir;
    let dir = fuchsia_fs::directory::open_directory(
        package_root,
        path,
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::DIRECTORY
            | fio::OpenFlags::POSIX_WRITABLE
            | fio::OpenFlags::POSIX_EXECUTABLE,
    )
    .await
    .expect("open directory");

    let (status, flags) = dir.get_flags().await.unwrap();
    let status = zx::Status::ok(status);

    let result = status.map(|()| flags);
    assert_eq!(result, Ok(expected_rights))
}

#[fuchsia::test]
async fn unsupported() {
    for source in dirs_to_test().await {
        unsupported_per_package_source(source).await
    }
}

async fn unsupported_per_package_source(source: PackageSource) {
    // Test unsupported APIs for root directory and subdirectory.
    assert_unsupported_directory_calls(&source, ".", "file").await;
    assert_unsupported_directory_calls(&source, ".", "dir").await;
    assert_unsupported_directory_calls(&source, ".", "meta").await;
    assert_unsupported_directory_calls(&source, "dir", "file").await;
    assert_unsupported_directory_calls(&source, "dir", "dir").await;

    // Test unsupported APIs for meta directory and subdirectory.
    assert_unsupported_directory_calls(&source, "meta", "file").await;
    assert_unsupported_directory_calls(&source, "meta", "dir").await;
    assert_unsupported_directory_calls(&source, "meta/dir", "file").await;
    assert_unsupported_directory_calls(&source, "meta/dir", "dir").await;
}

async fn assert_unsupported_directory_calls(
    source: &PackageSource,
    parent_path: &str,
    child_base_path: &str,
) {
    let parent = fuchsia_fs::directory::open_directory(
        &source.dir,
        parent_path,
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .expect("open parent directory");

    // Verify unlink() is not supported.
    assert_eq!(
        parent.unlink(child_base_path, &fio::UnlinkOptions::default()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );

    // get_token() should fail because the parent does not have the write right.
    assert_matches!(parent.get_token().await.expect("get_token fidl failed"),
                    (status, None) if status != 0);

    // Verify link() is not supported.  parent doesn't have the WRITE right, so this could fail with
    // BAD_HANDLE, but the token is bad so this could also fail with NOT_FOUND.  We don't care what
    // error we get just so long as we get one.
    assert_ne!(
        zx::Status::from_raw(
            parent.link(child_base_path, zx::Event::create().into(), "link").await.unwrap()
        ),
        zx::Status::OK
    );

    // Verify rename() is not supported.
    // Since we can't call GetToken, we can't construct a valid token to pass here.
    // But we can at least test what it does with an arbitrary event object.
    let token = zx::Event::create();
    assert_eq!(
        parent.rename(child_base_path, token, "renamed").await.unwrap(),
        Err(zx::sys::ZX_ERR_NOT_SUPPORTED)
    );

    // Verify watch() is not supported.
    let (_client, server) = fidl::endpoints::create_endpoints();
    assert_eq!(
        zx::Status::from_raw(parent.watch(fio::WatchMask::empty(), 0, server).await.unwrap()),
        zx::Status::NOT_SUPPORTED
    );

    // Verify nodeSetFlags() is not supported.
    assert_eq!(
        zx::Status::from_raw(parent.set_flags(fio::OpenFlags::empty()).await.unwrap()),
        zx::Status::NOT_SUPPORTED
    );
}
