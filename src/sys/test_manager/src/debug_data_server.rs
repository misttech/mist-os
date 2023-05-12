// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::constants,
    crate::run_events::RunEvent,
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::{create_proxy, create_request_stream, Proxy},
    fidl_fuchsia_io as fio, fidl_fuchsia_test_manager as ftest_manager, fuchsia_async as fasync,
    futures::{channel::mpsc, pin_mut, prelude::*, stream::FusedStream, StreamExt, TryStreamExt},
    std::path::{Path, PathBuf},
    tracing::warn,
};

struct DebugDataFile {
    pub name: String,
    pub contents: Vec<u8>,
}

async fn copy_kernel_debug_data(
    files: Vec<DebugDataFile>,
    dest_root_dir: PathBuf,
) -> Result<(), Error> {
    let read_write_flags: fuchsia_fs::OpenFlags =
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::RIGHT_WRITABLE;
    let overwite_file_flag: fuchsia_fs::OpenFlags = fuchsia_fs::OpenFlags::RIGHT_WRITABLE
        | fuchsia_fs::OpenFlags::RIGHT_READABLE
        | fuchsia_fs::OpenFlags::TRUNCATE
        | fuchsia_fs::OpenFlags::CREATE;

    let tmp_dir_root = fuchsia_fs::directory::open_in_namespace(
        dest_root_dir.to_str().unwrap(),
        read_write_flags,
    )?;
    let tmp_dir_root_ref = &tmp_dir_root;
    futures::stream::iter(files)
        .map(Ok)
        .try_for_each_concurrent(None, move |DebugDataFile { name, contents }| {
            let rel_file_path = PathBuf::from(&name);
            async move {
                if let Some(parent) = rel_file_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fuchsia_fs::directory::create_directory_recursive(
                            tmp_dir_root_ref,
                            parent.to_str().ok_or(anyhow!("Invalid path"))?,
                            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                        )
                        .await
                        .context("create subdirectories")?;
                    }
                }
                let file = fuchsia_fs::directory::open_file_no_describe(
                    tmp_dir_root_ref,
                    &name,
                    overwite_file_flag,
                )
                .context("open file")?;
                fuchsia_fs::file::write(&file, &contents).await.context("write file")?;
                Result::<_, Error>::Ok(())
            }
        })
        .await?;

    Ok(())
}

const DEBUG_DATA_TIMEOUT_SECONDS: i64 = 15;
const DEBUG_DATA_PATH: &'static str = "/debugdata";

// TODO(fxbug.dev/110062): Once scp is no longer needed this can just call serve_iterator instead.
pub(crate) async fn send_kernel_debug_data(
    mut event_sender: mpsc::Sender<RunEvent>,
    accumulate: bool,
) {
    if !accumulate {
        tracing::info!("Serving kernel debug data");
        // If we're not accumulating, we don't need the custom copy logic.
        if let Err(e) = serve_iterator(DEBUG_DATA_PATH, event_sender).await {
            warn!("Error serving kernel debug data: {:?}", e);
        }
        return;
    }

    let root_dir = match fuchsia_fs::directory::open_in_namespace(
        DEBUG_DATA_PATH,
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    ) {
        Ok(dir) => dir,
        Err(err) => {
            warn!("Failed to open '/debugdata'. Error: {}", err);
            return;
        }
    };

    let files = fuchsia_fs::directory::readdir_recursive(
        &root_dir,
        Some(fasync::Duration::from_seconds(DEBUG_DATA_TIMEOUT_SECONDS)),
    )
    .filter_map(|result| async move {
        match result {
            Ok(entry) => {
                if entry.kind != fio::DirentType::File {
                    None
                } else {
                    Some(entry)
                }
            }
            Err(err) => {
                warn!("Error while reading directory entry. Error: {}", err);
                None
            }
        }
    })
    .filter_map(|entry| async move {
        let path = PathBuf::from(DEBUG_DATA_PATH).join(entry.name).to_string_lossy().to_string();
        match fuchsia_fs::file::open_in_namespace(&path, fuchsia_fs::OpenFlags::RIGHT_READABLE) {
            Ok(file) => Some((path, file)),
            Err(err) => {
                warn!("Failed to read file {}. Error {}", path, err);
                None
            }
        }
    })
    .filter_map(|(path, file)| async move {
        match fuchsia_fs::file::read(&file).await {
            Ok(contents) => Some((path, contents)),
            Err(err) => {
                warn!("Failed to read file {}. Error {}", path, err);
                None
            }
        }
    })
    .map(|(name, contents)| {
        // Remove the leading '/' so there is no 'root' entry.
        DebugDataFile { name: name.trim_start_matches('/').to_string(), contents }
    })
    .collect::<Vec<DebugDataFile>>()
    .await;

    if !files.is_empty() {
        tracing::info!(
            "Found early boot profile files: {:?}",
            files
                .iter()
                .map(|file| format!("file: {}, content_len: {:?}", file.name, file.contents.len()))
                .collect::<Vec<_>>()
        );
        // We copy the files to a well known directory in /tmp. This supports exporting the
        // files off device via SCP. Once this flow is no longer needed, we can use something
        // like an ephemeral directory which is torn down once we're done instead.
        copy_kernel_debug_data(
            files,
            Path::new(constants::KERNEL_DEBUG_DATA_FOR_SCP).to_path_buf(),
        )
        .await
        .unwrap_or_else(|e| warn!("Error copying kernel debug data: {:?}", e));
        // deliberately hold event_sender open even though we aren't sending anything... this
        // keeps RunController channel open, and run-test-suite running until copying files to
        // tmp/ is complete. See fxbug.dev/14996#c11 for details.
        event_sender.disconnect();
    }
}

const ITERATOR_BATCH_SIZE: usize = 10;

async fn filter_map_filename(
    entry_result: Result<
        fuchsia_fs::directory::DirEntry,
        fuchsia_fs::directory::RecursiveEnumerateError,
    >,
    dir_path: &str,
) -> Option<String> {
    match entry_result {
        Ok(fuchsia_fs::directory::DirEntry { name, kind }) => match kind {
            fuchsia_fs::directory::DirentKind::File => Some(name),
            _ => None,
        },
        Err(e) => {
            warn!("Error reading directory in {}: {:?}", dir_path, e);
            None
        }
    }
}

async fn serve_file_over_socket(file: fio::FileProxy, socket: fuchsia_zircon::Socket) {
    let mut socket = fasync::Socket::from_socket(socket).unwrap();

    // We keep a buffer of 4.8 MB while reading the file
    let num_bytes: u64 = 1024 * 48;
    let (mut sender, mut recv) = mpsc::channel(100);
    let _file_read_task = fasync::Task::spawn(async move {
        loop {
            let bytes = fuchsia_fs::file::read_num_bytes(&file, num_bytes).await.unwrap();
            let len = bytes.len();
            if let Err(_) = sender.send(bytes).await {
                // no recv, don't read rest of the file.
                break;
            }
            if len != usize::try_from(num_bytes).unwrap() {
                // done reading file
                break;
            }
        }
    });

    while let Some(bytes) = recv.next().await {
        if let Err(e) = socket.write_all(bytes.as_slice()).await {
            warn!("cannot serve file: {:?}", e);
            return;
        }
    }
}

/// Serves the |DebugDataIterator| protocol by serving all the files contained under
/// |dir_path|.
///
/// The contents under |dir_path| are assumed to not change while the iterator is served.
pub(crate) async fn serve_iterator(
    dir_path: &str,
    mut event_sender: mpsc::Sender<RunEvent>,
) -> Result<(), Error> {
    let directory =
        fuchsia_fs::directory::open_in_namespace(dir_path, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    let file_stream = fuchsia_fs::directory::readdir_recursive(
        &directory,
        Some(fasync::Duration::from_seconds(DEBUG_DATA_TIMEOUT_SECONDS)),
    )
    .filter_map(|entry| filter_map_filename(entry, dir_path))
    .peekable();
    pin_mut!(file_stream);

    if file_stream.as_mut().peek().await.is_none() {
        // No files to serve.
        return Ok(());
    }

    let mut file_stream = file_stream.fuse();

    let (client, mut iterator) = create_request_stream::<ftest_manager::DebugDataIteratorMarker>()?;
    let _ = event_sender.send(RunEvent::debug_data(client).into()).await;
    event_sender.disconnect(); // No need to hold this open while we serve the iterator.

    while let Some(request) = iterator.try_next().await? {
        let ftest_manager::DebugDataIteratorRequest::GetNext { responder } = request;
        let next_files = match file_stream.is_terminated() {
            true => vec![],
            false => file_stream.by_ref().take(ITERATOR_BATCH_SIZE).collect().await,
        };
        let debug_data = next_files
            .into_iter()
            .map(|file_name| {
                let (file, server) = create_proxy::<fio::NodeMarker>().unwrap();
                let file = fio::FileProxy::new(file.into_channel().unwrap());
                directory.open(
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                    fio::ModeType::empty(),
                    &file_name,
                    server,
                )?;

                tracing::info!("Serving debug data file {}: {}", dir_path, file_name);
                let (client, server) = fuchsia_zircon::Socket::create_stream();
                fasync::Task::spawn(serve_file_over_socket(file, server)).detach();
                Ok(ftest_manager::DebugData {
                    socket: Some(client.into()),
                    name: file_name.into(),
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let _ = responder.send(debug_data);
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*, crate::run_events::RunEventPayload, fuchsia_async as fasync,
        std::collections::HashSet, tempfile::tempdir, test_diagnostics::collect_string_from_socket,
    };

    #[fuchsia::test]
    async fn copy_empty() {
        let dir = tempdir().unwrap();
        copy_kernel_debug_data(vec![], dir.path().to_path_buf()).await.unwrap();
        let contents = std::fs::read_dir(dir.path()).unwrap().collect::<Vec<_>>();
        assert!(contents.is_empty());
    }

    #[fuchsia::test]
    async fn single_file() {
        let dir = tempdir().unwrap();
        copy_kernel_debug_data(
            vec![DebugDataFile { name: "file".to_string(), contents: b"test".to_vec() }],
            dir.path().to_path_buf(),
        )
        .await
        .unwrap();
        let contents = std::fs::read_dir(dir.path()).unwrap().collect::<Vec<_>>();
        assert_eq!(contents.len(), 1);

        assert_eq!(std::fs::read(dir.path().join("file")).unwrap().as_slice(), b"test");
    }

    #[fuchsia::test]
    async fn multiple_files() {
        let dir = tempdir().unwrap();
        copy_kernel_debug_data(
            vec![
                DebugDataFile { name: "file".to_string(), contents: b"test".to_vec() },
                DebugDataFile { name: "dir/file2".to_string(), contents: b"test2".to_vec() },
            ],
            dir.path().to_path_buf(),
        )
        .await
        .unwrap();
        let contents = std::fs::read_dir(dir.path()).unwrap().collect::<Vec<_>>();
        assert_eq!(contents.len(), 2);

        assert_eq!(std::fs::read(dir.path().join("file")).unwrap().as_slice(), b"test");
        assert_eq!(std::fs::read(dir.path().join("dir/file2")).unwrap().as_slice(), b"test2");
    }

    async fn serve_iterator_from_tmp(
        dir: &tempfile::TempDir,
    ) -> (Option<ftest_manager::DebugDataIteratorProxy>, fasync::Task<Result<(), Error>>) {
        let (send, mut recv) = mpsc::channel(0);
        let dir_path = dir.path().to_str().unwrap().to_string();
        let task = fasync::Task::local(async move { serve_iterator(&dir_path, send).await });
        let proxy = recv.next().await.map(|event| {
            let RunEventPayload::DebugData(client) = event.into_payload();
            client.into_proxy().expect("into proxy")
        });
        (proxy, task)
    }

    #[fuchsia::test]
    async fn serve_iterator_empty_dir_returns_no_client() {
        let dir = tempdir().unwrap();
        let (client, task) = serve_iterator_from_tmp(&dir).await;
        assert!(client.is_none());
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_single_response() {
        let dir = tempdir().unwrap();
        fuchsia_fs::file::write_in_namespace(&dir.path().join("file").to_string_lossy(), "test")
            .await
            .expect("write to file");

        let (client, task) = serve_iterator_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut values = proxy.get_next().await.expect("get next");
        assert_eq!(1usize, values.len());
        let ftest_manager::DebugData { name, socket, .. } = values.pop().unwrap();
        assert_eq!(Some("file".to_string()), name);
        let contents = collect_string_from_socket(socket.unwrap()).await.expect("read socket");
        assert_eq!("test", contents);

        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        // Calling again is okay and should also return empty vector.
        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        drop(proxy);
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_multiple_responses() {
        let num_files_served = ITERATOR_BATCH_SIZE * 2;

        let dir = tempdir().unwrap();
        for idx in 0..num_files_served {
            fuchsia_fs::file::write_in_namespace(
                &dir.path().join(format!("file-{:?}", idx)).to_string_lossy(),
                &format!("test-{:?}", idx),
            )
            .await
            .expect("write to file");
        }

        let (client, task) = serve_iterator_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut all_files = vec![];
        loop {
            let mut next = proxy.get_next().await.expect("get next");
            if next.is_empty() {
                break;
            }
            all_files.append(&mut next);
        }

        let file_contents: HashSet<_> = futures::stream::iter(all_files)
            .then(|ftest_manager::DebugData { name, socket, .. }| async move {
                let contents =
                    collect_string_from_socket(socket.unwrap()).await.expect("read socket");
                (name.unwrap(), contents)
            })
            .collect()
            .await;

        let expected_files: HashSet<_> = (0..num_files_served)
            .map(|idx| (format!("file-{:?}", idx), format!("test-{:?}", idx)))
            .collect();

        assert_eq!(file_contents, expected_files);
        drop(proxy);
        task.await.expect("iterator server should not fail");
    }
}
