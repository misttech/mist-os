// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::SimpleFile;

// Macros are exported into the root of the crate.
use crate::{
    assert_close, assert_event, assert_get_attr, assert_read, assert_read_at, assert_read_at_err,
    assert_read_err, assert_read_fidl_err_closed, assert_seek, assert_truncate,
    assert_truncate_err, assert_write, assert_write_at, assert_write_at_err, assert_write_err,
    assert_write_fidl_err_closed, clone_as_file_assert_err, clone_get_proxy_assert,
};

use crate::{execution_scope::ExecutionScope, file::test_utils::*, ToObjectRequest};

use {fidl::endpoints::create_proxy, fidl_fuchsia_io as fio, fuchsia_async::TestExecutor};

const S_IRUSR: u32 = libc::S_IRUSR as u32;
const S_IWUSR: u32 = libc::S_IWUSR as u32;

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
        let (proxy, server_end) =
            create_proxy::<fio::FileMarker>().expect("Failed to create connection endpoints");

        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE;
        flags
            .to_object_request(server_end)
            .handle(|object_request| vfs::file::serve(server, scope, &flags, object_request));

        assert_event!(proxy, fio::FileEvent::OnOpen_ { s, info }, {
            assert_eq!(s, fuchsia_zircon_status::Status::OK.into_raw());
            let info = *info.expect("Empty fio::NodeInfoDeprecated");
            assert!(matches!(
                info,
                fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: None }),
            ));
        });
    });
}

#[test]
fn read_write_no_write_flag() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        SimpleFile::read_write(b"Can read"),
        |proxy| async move {
            assert_read!(proxy, "Can read");
            assert_write_err!(proxy, "Can write", Status::BAD_HANDLE);
            assert_write_at_err!(proxy, 0, "Can write", Status::BAD_HANDLE);
            assert_seek!(proxy, 0, Start);
            assert_read!(proxy, "Can read");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_write_no_read_flag() {
    run_server_client(
        fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(""),
        |proxy| async move {
            assert_read_err!(proxy, Status::BAD_HANDLE);
            assert_read_at_err!(proxy, 0, Status::BAD_HANDLE);
            assert_write!(proxy, "Can write");
            assert_close!(proxy);
        },
    );
}

#[test]
fn open_truncate() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::TRUNCATE,
        SimpleFile::read_write(b"Will be erased"),
        |proxy| {
            async move {
                // Seek to the end to check the current size.
                assert_seek!(proxy, 0, End, Ok(0));
                assert_write!(proxy, "File content");
                // Ensure remaining contents to not leak out of read call.
                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, "File content");
                assert_close!(proxy);
            }
        },
    );
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
fn write_at_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"File content"),
        |proxy| async move {
            assert_write_at!(proxy, 0, "New content!");
            assert_seek!(proxy, 0, Start);
            // Validate contents.
            assert_read!(proxy, "New content!");
            assert_close!(proxy);
        },
    );
}

#[test]
fn write_at_overlapping() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"012345678901234567"),
        |proxy| async move {
            assert_write_at!(proxy, 8, "le content");
            assert_write_at!(proxy, 6, "file");
            assert_write_at!(proxy, 0, "Whole file");
            // Validate contents.
            assert_seek!(proxy, 0, Start);
            assert_read!(proxy, "Whole file content");
            assert_close!(proxy);
        },
    );
}

#[test]
fn write_mixed_with_write_at() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"012345678901234567"),
        |proxy| async move {
            assert_write!(proxy, "whole");
            assert_write_at!(proxy, 0, "Who");
            assert_write!(proxy, " 1234 ");
            assert_write_at!(proxy, 6, "file");
            assert_write!(proxy, "content");
            // Validate contents.
            assert_seek!(proxy, 0, Start);
            assert_read!(proxy, "Whole file content");
            assert_close!(proxy);
        },
    );
}

#[test]
fn appending_writes() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::APPEND,
        SimpleFile::read_write(b"data-1"),
        |proxy| async move {
            assert_write!(proxy, " data-2");
            assert_read_at!(proxy, 0, "data-1 data-2");

            // The seek offset is reset to the end of the file when writing.
            assert_seek!(proxy, 0, Start);
            assert_write!(proxy, " data-3");
            assert_read_at!(proxy, 0, "data-1 data-2 data-3");

            // The seek offset is reset to the end of the file when writing after a truncate.
            assert_truncate!(proxy, 6);
            assert_write!(proxy, " data-4");
            assert_read_at!(proxy, 0, "data-1 data-4");
            assert_seek!(proxy, 0, End, Ok(13));

            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_read_write() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Initial"),
        |proxy| {
            async move {
                assert_read!(proxy, "Init");
                assert_write!(proxy, "l con");
                // buffer: "Initl con"
                assert_seek!(proxy, 0, Start);
                assert_write!(proxy, "Fina");
                // buffer: "Final con"
                assert_seek!(proxy, 0, End, Ok(9));
                assert_write!(proxy, "tent");
                // Validate contents.
                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, "Final content");
                assert_close!(proxy);
            }
        },
    );
}

#[test]
fn write_after_seek_beyond_size_fills_gap_with_zeroes() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Before gap"),
        |proxy| {
            async move {
                assert_seek!(proxy, 0, End, Ok(10));
                assert_seek!(proxy, 4, Current, Ok(14)); // Four byte gap past original content.
                assert_write!(proxy, "After gap");
                // Validate contents.
                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, "Before gap\0\0\0\0After gap");
                assert_close!(proxy);
            }
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
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Content"),
        |proxy| {
            async move {
                assert_seek!(proxy, 7, Start);
                assert_read!(proxy, "");
                assert_write!(proxy, " ext");
                //      "Content ext"));
                assert_seek!(proxy, 3, Current, Ok(14));
                assert_write!(proxy, "ed");
                //      "Content ext000ed"));
                assert_seek!(proxy, 4, End, Ok(20));
                assert_write!(proxy, "ther");
                //      "Content ext000ed0000ther"));
                //       0         1         2
                //       012345678901234567890123
                assert_seek!(proxy, 11, Start);
                assert_write!(proxy, "end");
                assert_seek!(proxy, 16, Start);
                assert_write!(proxy, " fur");
                // Validate contents.
                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, "Content extended further");
                assert_close!(proxy);
            }
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
fn seek_after_expanding_truncate() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Content"),
        |proxy| async move {
            assert_truncate!(proxy, 12); // Increases size of the file to 12, padding with zeroes.
            assert_seek!(proxy, 10, Start);
            assert_write!(proxy, "end");
            // Validate contents.
            assert_seek!(proxy, 0, Start);
            assert_read!(proxy, "Content\0\0\0end");
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_beyond_size_after_shrinking_truncate() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Content"),
        |proxy| async move {
            assert_truncate!(proxy, 4); // Decrease the size of the file to four.
            assert_seek!(proxy, 0, End, Ok(4));
            assert_seek!(proxy, 4, Current, Ok(8)); // Four bytes beyond the truncated end.
            assert_write!(proxy, "end");
            // Validate contents.
            assert_seek!(proxy, 0, Start);
            assert_read!(proxy, "Cont\0\0\0\0end");
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
fn truncate_to_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Content"),
        |proxy| {
            async move {
                assert_read!(proxy, "Content");
                assert_truncate!(proxy, 0);
                // truncate should not change the seek position.
                assert_seek!(proxy, 0, Current, Ok(7));
                assert_seek!(proxy, 0, Start);
                assert_write!(proxy, "Replaced");
                // Validate contents.
                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, "Replaced");
                assert_close!(proxy);
            }
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
fn get_attr_read_write() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        SimpleFile::read_write(b"Content"),
        |proxy| async move {
            assert_get_attr!(
                proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_FILE | S_IWUSR | S_IRUSR,
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
