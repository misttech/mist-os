// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::SimpleFile;
use crate::execution_scope::ExecutionScope;
use crate::file::test_utils::*;
use crate::{
    assert_close, assert_event, assert_get_attr, assert_read, assert_read_at,
    assert_read_fidl_err_closed, assert_seek, assert_truncate_err, assert_write_err,
    assert_write_fidl_err_closed, clone_as_file_assert_err, clone_get_proxy_assert,
    ToObjectRequest,
};
use assert_matches::assert_matches;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use fuchsia_async::TestExecutor;
use futures::StreamExt;
use zx_status::Status;

const S_IRUSR: u32 = libc::S_IRUSR as u32;

/// Verify that [`SimpleFile::read_only`] works with static and owned data. Compile-time test.
#[test]
fn read_only_types() {
    // Static data.
    SimpleFile::read_only("from str");
    SimpleFile::read_only(b"from bytes");

    const STATIC_STRING: &'static str = "static string";
    const STATIC_BYTES: &'static [u8] = b"static bytes";
    SimpleFile::read_only(STATIC_STRING);
    SimpleFile::read_only(&STATIC_STRING);
    SimpleFile::read_only(STATIC_BYTES);
    SimpleFile::read_only(&STATIC_BYTES);

    // Owned data.
    SimpleFile::read_only(String::from("Hello, world"));
    SimpleFile::read_only(vec![0u8; 2]);

    // Borrowed data.
    let runtime_string = String::from("Hello, world");
    SimpleFile::read_only(&runtime_string);
    let runtime_bytes = vec![0u8; 2];
    SimpleFile::read_only(&runtime_bytes);
}

#[test]
fn read_only_read_static() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Read only test"),
        |proxy| async move {
            assert_read!(proxy, "Read only test");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_read_owned() {
    let bytes = String::from("Run-time value");
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(bytes),
        |proxy| async move {
            assert_read!(proxy, "Run-time value");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_read() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Read only test"),
        |proxy| async move {
            assert_read!(proxy, "Read only test");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_ignore_posix_flag() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE,
        SimpleFile::read_only(b"Content"),
        |proxy| async move {
            assert_read!(proxy, "Content");
            assert_write_err!(proxy, "Can write", Status::BAD_HANDLE);
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_read_with_describe() {
    let exec = TestExecutor::new();
    let scope = ExecutionScope::new();

    let server = SimpleFile::read_only(b"Read only test");

    run_client(exec, || async move {
        let (proxy, server_end) = create_proxy::<fio::FileMarker>();

        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE;
        flags
            .to_object_request(server_end)
            .handle(|object_request| vfs::file::serve(server, scope, &flags, object_request));

        assert_event!(proxy, fio::FileEvent::OnOpen_ { s, info }, {
            assert_eq!(s, zx_status::Status::OK.into_raw());
            let info = *info.expect("Empty fio::NodeInfoDeprecated");
            assert!(matches!(
                info,
                fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: None }),
            ));
        });
    });
}

#[test]
fn read_only_write_is_not_supported() {
    let exec = TestExecutor::new();
    let scope = ExecutionScope::new();

    let server = SimpleFile::read_only(b"Read only test");

    run_client(exec, || async move {
        let (proxy, server_end) = create_proxy::<fio::FileMarker>();

        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
        flags
            .to_object_request(server_end)
            .handle(|object_request| vfs::file::serve(server, scope, &flags, object_request));

        let mut event_stream = proxy.take_event_stream();
        assert_matches!(
            event_stream.next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::ACCESS_DENIED, .. }))
        );
    });
}

#[test]
fn read_at_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Whole file content"),
        |proxy| async move {
            assert_read_at!(proxy, 0, "Whole file content");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_at_overlapping() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Content of the file"),
        //                 0         1
        //                 0123456789012345678
        |proxy| async move {
            assert_read_at!(proxy, 3, "tent of the");
            assert_read_at!(proxy, 11, "the file");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_mixed_with_read_at() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Content of the file"),
        //                 0         1
        //                 0123456789012345678
        |proxy| async move {
            assert_read!(proxy, "Content");
            assert_read_at!(proxy, 3, "tent of the");
            assert_read!(proxy, " of the ");
            assert_read_at!(proxy, 11, "the file");
            assert_read!(proxy, "file");
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_valid_positions() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Long file content"),
        //                 0         1
        //                 01234567890123456
        |proxy| async move {
            assert_seek!(proxy, 5, Start);
            assert_read!(proxy, "file");
            assert_seek!(proxy, 1, Current, Ok(10));
            assert_read!(proxy, "content");
            assert_seek!(proxy, -12, End, Ok(5));
            assert_read!(proxy, "file content");
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_valid_beyond_size() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Content"),
        |proxy| async move {
            assert_seek!(proxy, 20, Start);
        },
    );
}

#[test]
fn seek_triggers_overflow() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"File size and contents don't matter for this test"),
        |proxy| async move {
            assert_seek!(proxy, i64::MAX, Start);
            assert_seek!(proxy, i64::MAX, Current, Ok(u64::MAX - 1));
            assert_seek!(proxy, 2, Current, Err(Status::OUT_OF_RANGE));
        },
    );
}

#[test]
fn seek_invalid_before_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(
            b"Seek position is unaffected",
            // 0        1         2
            // 12345678901234567890123456
        ),
        |proxy| async move {
            assert_seek!(proxy, -10, Current, Err(Status::OUT_OF_RANGE));
            assert_read!(proxy, "Seek");
            assert_seek!(proxy, -10, Current, Err(Status::OUT_OF_RANGE));
            assert_read!(proxy, " position");
            assert_seek!(proxy, -100, End, Err(Status::OUT_OF_RANGE));
            assert_read!(proxy, " is unaffected");
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_empty_file() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b""),
        |proxy| async move {
            assert_seek!(proxy, 0, Start);
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_allowed_beyond_size() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(
            b"Long content",
            // 0        1
            // 12345678901
        ),
        |proxy| async move {
            assert_seek!(proxy, 100, Start);
            assert_close!(proxy);
        },
    );
}

#[test]
fn truncate_read_only_file() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Read-only content"),
        |proxy| async move {
            assert_truncate_err!(proxy, 10, Status::BAD_HANDLE);
            assert_close!(proxy);
        },
    );
}

#[test]
fn get_attr_read_only() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Content"),
        |proxy| async move {
            assert_get_attr!(
                proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_FILE | S_IRUSR,
                    id: fio::INO_UNKNOWN,
                    content_size: 7,
                    storage_size: 7,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            );
            assert_close!(proxy);
        },
    );
}

#[test]
fn clone_cannot_increase_access() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_only(b"Initial content"),
        |first_proxy| async move {
            assert_read!(first_proxy, "Initial content");
            assert_write_err!(first_proxy, "Write attempt", Status::BAD_HANDLE);

            let second_proxy = clone_as_file_assert_err!(
                &first_proxy,
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DESCRIBE,
                Status::ACCESS_DENIED
            );

            assert_read_fidl_err_closed!(second_proxy);
            assert_write_fidl_err_closed!(second_proxy, "Write attempt");

            assert_close!(first_proxy);
        },
    );
}
