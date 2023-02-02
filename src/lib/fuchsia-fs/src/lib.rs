// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! fuchsia.IO UTIL-ity library

use fidl_fuchsia_io as fio;

pub mod directory;
pub mod file;
pub mod node;

// Reexported from fidl_fuchsia_io for convenience
pub use fio::OpenFlags;

/// canonicalize_path will remove a leading `/` if it exists, since it's always unnecessary and in
/// some cases disallowed (fxbug.dev/28436).
pub fn canonicalize_path(path: &str) -> &str {
    if path == "/" {
        return ".";
    }
    if path.starts_with('/') {
        return &path[1..];
    }
    path
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::Error,
        fidl::endpoints::ServerEnd,
        fuchsia_async as fasync, fuchsia_zircon_status as zx_status,
        std::{fs, path::Path},
        tempfile::TempDir,
        vfs::{
            directory::entry::DirectoryEntry,
            execution_scope::ExecutionScope,
            file::vmo::{read_only_static, read_write, simple_init_vmo_with_capacity},
            pseudo_directory,
        },
    };

    #[fasync::run_singlethreaded(test)]
    async fn open_and_read_file_test() {
        let tempdir = TempDir::new().expect("failed to create tmp dir");
        let data = "abc".repeat(10000);
        fs::write(tempdir.path().join("myfile"), &data).expect("failed writing file");

        let dir = crate::directory::open_in_namespace(
            tempdir.path().to_str().unwrap(),
            OpenFlags::RIGHT_READABLE,
        )
        .expect("could not open tmp dir");
        let file = directory::open_file_no_describe(&dir, "myfile", OpenFlags::RIGHT_READABLE)
            .expect("could not open file");
        let contents = file::read_to_string(&file).await.expect("could not read file");
        assert_eq!(&contents, &data, "File contents did not match");
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_and_write_file_test() {
        // Create temp dir for test.
        let tempdir = TempDir::new().expect("failed to create tmp dir");
        let dir = crate::directory::open_in_namespace(
            tempdir.path().to_str().unwrap(),
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
        )
        .expect("could not open tmp dir");

        // Write contents.
        let file_name = Path::new("myfile");
        let data = "abc".repeat(10000);
        let file = directory::open_file_no_describe(
            &dir,
            file_name.to_str().unwrap(),
            OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
        )
        .expect("could not open file");
        file::write(&file, &data).await.expect("could not write file");

        // Verify contents.
        let contents = std::fs::read_to_string(tempdir.path().join(file_name)).unwrap();
        assert_eq!(&contents, &data, "File contents did not match");
    }

    #[test]
    fn test_canonicalize_path() {
        assert_eq!(canonicalize_path("/"), ".");
        assert_eq!(canonicalize_path("/foo"), "foo");
        assert_eq!(canonicalize_path("/foo/bar/"), "foo/bar/");

        assert_eq!(canonicalize_path("."), ".");
        assert_eq!(canonicalize_path("./"), "./");
        assert_eq!(canonicalize_path("foo/bar/"), "foo/bar/");
    }

    #[fasync::run_until_stalled(test)]
    async fn flags_test() -> Result<(), Error> {
        let example_dir = pseudo_directory! {
            "read_only" => read_only_static("read_only"),
            "read_write" => read_write(
                simple_init_vmo_with_capacity("read_write".as_bytes(), 100)
            ),
        };
        let (example_dir_proxy, example_dir_service) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
        let scope = ExecutionScope::new();
        example_dir.open(
            scope,
            OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE | OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            ServerEnd::new(example_dir_service.into_channel()),
        );

        for (file_name, flags, should_succeed) in vec![
            ("read_only", OpenFlags::RIGHT_READABLE, true),
            ("read_only", OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE, false),
            ("read_only", OpenFlags::RIGHT_WRITABLE, false),
            ("read_write", OpenFlags::RIGHT_READABLE, true),
            ("read_write", OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE, true),
            ("read_write", OpenFlags::RIGHT_WRITABLE, true),
        ] {
            let file_proxy =
                directory::open_file_no_describe(&example_dir_proxy, file_name, flags)?;
            match (should_succeed, file_proxy.query().await) {
                (true, Ok(_)) => (),
                (false, Err(_)) => continue,
                (true, Err(e)) => {
                    panic!("failed to open when expected success, couldn't describe: {:?}", e)
                }
                (false, Ok(d)) => {
                    panic!("successfully opened when expected failure, could describe: {:?}", d)
                }
            }
            if flags.intersects(OpenFlags::RIGHT_READABLE) {
                assert_eq!(
                    file_name,
                    file::read_to_string(&file_proxy).await.expect("failed to read file")
                );
            }
            if flags.intersects(OpenFlags::RIGHT_WRITABLE) {
                let _: u64 = file_proxy
                    .write(b"write_only")
                    .await
                    .expect("write failed")
                    .map_err(zx_status::Status::from_raw)
                    .expect("write error");
            }
            assert_eq!(file_proxy.close().await?, Ok(()));
        }
        Ok(())
    }
}
