// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the asynchronous files.

use super::{read_only, read_write, VmoFile};

// Macros are exported into the root of the crate.
use crate::{
    assert_close, assert_event, assert_get_attr, assert_get_vmo, assert_read, assert_read_at,
    assert_read_at_err, assert_read_err, assert_read_fidl_err_closed, assert_seek, assert_truncate,
    assert_truncate_err, assert_vmo_content, assert_write, assert_write_at, assert_write_at_err,
    assert_write_err, assert_write_fidl_err_closed, clone_as_file_assert_err,
    clone_get_proxy_assert, clone_get_vmo_file_proxy_assert_err,
    clone_get_vmo_file_proxy_assert_ok,
};

use crate::execution_scope::ExecutionScope;
use crate::file::test_utils::*;
use crate::ToObjectRequest;

use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use fuchsia_async::TestExecutor;
use fuchsia_zircon::sys::ZX_OK;
use fuchsia_zircon::Vmo;
use futures::channel::oneshot;
use futures::future::join;
use libc::{S_IRUSR, S_IWUSR};

/// Verify that [`read_only`] works with static and owned data. Compile-time test.
#[test]
fn read_only_types() {
    // Static data.
    read_only("from str");
    read_only(b"from bytes");

    const STATIC_STRING: &'static str = "static string";
    const STATIC_BYTES: &'static [u8] = b"static bytes";
    read_only(STATIC_STRING);
    read_only(&STATIC_STRING);
    read_only(STATIC_BYTES);
    read_only(&STATIC_BYTES);

    // Owned data.
    read_only(String::from("Hello, world"));
    read_only(vec![0u8; 2]);

    // Borrowed data.
    let runtime_string = String::from("Hello, world");
    read_only(&runtime_string);
    let runtime_bytes = vec![0u8; 2];
    read_only(&runtime_bytes);
}

#[test]
fn read_only_read_static() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(b"Read only test"),
        |proxy| async move {
            assert_read!(proxy, "Read only test");
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_read_owned() {
    let bytes = String::from("Run-time value");
    run_server_client(fio::OpenFlags::RIGHT_READABLE, read_only(bytes), |proxy| async move {
        assert_read!(proxy, "Run-time value");
        assert_close!(proxy);
    });
}

#[test]
fn read_only_read() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(b"Read only test"),
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
        read_only(b"Content"),
        |proxy| async move {
            assert_read!(proxy, "Content");
            assert_write_err!(proxy, "Can write", Status::BAD_HANDLE);
            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_read_no_status() {
    let (check_event_send, check_event_recv) = oneshot::channel::<()>();

    test_server_client(fio::OpenFlags::RIGHT_READABLE, read_only(b"Read only test"), |proxy| {
        async move {
            use futures::{FutureExt as _, StreamExt as _};
            // Make sure `open()` call is complete, before we start checking.
            check_event_recv.await.unwrap();
            assert_matches::assert_matches!(proxy.take_event_stream().next().now_or_never(), None);
        }
    })
    .coordinator(|mut controller| {
        controller.run_until_stalled();
        check_event_send.send(()).unwrap();
        controller.run_until_complete();
    })
    .run();
}

#[test]
fn read_only_read_with_describe() {
    let exec = TestExecutor::new();
    let scope = ExecutionScope::new();

    let server = read_only(b"Read only test");

    run_client(exec, || async move {
        let (proxy, server_end) =
            create_proxy::<fio::FileMarker>().expect("Failed to create connection endpoints");

        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE;
        flags
            .to_object_request(server_end)
            .handle(|object_request| vfs::file::serve(server, scope, &flags, object_request));

        assert_event!(proxy, fio::FileEvent::OnOpen_ { s, info }, {
            assert_eq!(s, ZX_OK);
            let info = *info.expect("Empty fio::NodeInfoDeprecated");
            assert!(matches!(
                info,
                fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: Some(_) }),
            ));
        });
    });
}

#[test]
fn read_write_no_write_flag() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_write(b"Can read"),
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
    run_server_client(fio::OpenFlags::RIGHT_WRITABLE, read_write(""), |proxy| async move {
        assert_read_err!(proxy, Status::BAD_HANDLE);
        assert_read_at_err!(proxy, 0, Status::BAD_HANDLE);
        assert_write!(proxy, "Can write");
        assert_close!(proxy);
    });
}

#[test]
fn open_truncate() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::TRUNCATE,
        read_write(b"Will be erased"),
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
        read_only(b"Whole file content"),
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
        read_only(b"Content of the file"),
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
        read_only(b"Content of the file"),
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
fn executable_read() {
    const FILE_CONTENTS: &'static str = "file-contents";
    let vmo = Vmo::create(FILE_CONTENTS.len() as u64).unwrap();
    vmo.write(FILE_CONTENTS.as_bytes(), 0).unwrap();

    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        VmoFile::new(
            vmo, /*readable*/ true, /*writable*/ false, /*executable*/ true,
        ),
        |proxy| async move {
            assert_read!(proxy, FILE_CONTENTS);
            assert_close!(proxy);
        },
    );
}

#[test]
fn write_at_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"File content"),
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
        read_write(b"012345678901234567"),
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
        read_write(b"012345678901234567"),
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
        read_write(b"data-1"),
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
        read_write(b"Initial"),
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
        read_write(b"Before gap"),
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
        read_only(b"Long file content"),
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
        read_write(b"Content"),
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
        read_only(b"File size and contents don't matter for this test"),
        |proxy| async move {
            assert_seek!(proxy, i64::MAX, Start);
            assert_seek!(proxy, i64::MAX, Current, Ok(u64::MAX - 1));
            assert_seek!(proxy, 2, Current, Err(Status::INVALID_ARGS));
        },
    );
}

#[test]
fn seek_invalid_before_0() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(
            b"Seek position is unaffected",
            // 0        1         2
            // 12345678901234567890123456
        ),
        |proxy| async move {
            assert_seek!(proxy, -10, Current, Err(Status::INVALID_ARGS));
            assert_read!(proxy, "Seek");
            assert_seek!(proxy, -10, Current, Err(Status::INVALID_ARGS));
            assert_read!(proxy, " position");
            assert_seek!(proxy, -100, End, Err(Status::INVALID_ARGS));
            assert_read!(proxy, " is unaffected");
            assert_close!(proxy);
        },
    );
}

#[test]
fn seek_after_expanding_truncate() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Content"),
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
        read_write(b"Content"),
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
    run_server_client(fio::OpenFlags::RIGHT_READABLE, read_only(b""), |proxy| async move {
        assert_seek!(proxy, 0, Start);
        assert_close!(proxy);
    });
}

#[test]
fn seek_allowed_beyond_size() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(
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
        read_write(b"Content"),
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
        read_only(b"Read-only content"),
        |proxy| async move {
            assert_truncate_err!(proxy, 10, Status::BAD_HANDLE);
            assert_close!(proxy);
        },
    );
}

#[test]
fn clone_reduce_access() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Initial content"),
        |first_proxy| async move {
            assert_read!(first_proxy, "Initial content");
            assert_truncate!(first_proxy, 0);
            assert_seek!(first_proxy, 0, Start);
            assert_write!(first_proxy, "As updated");

            let second_proxy = clone_get_vmo_file_proxy_assert_ok!(
                &first_proxy,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE
            );

            assert_read!(second_proxy, "As updated");
            assert_truncate_err!(second_proxy, 0, Status::BAD_HANDLE);
            assert_write_err!(second_proxy, "Overwritten", Status::BAD_HANDLE);

            assert_close!(first_proxy);
        },
    );
}

#[test]
fn clone_inherit_access() {
    use fidl_fuchsia_io as fio;

    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Initial content"),
        |first_proxy| async move {
            assert_read!(first_proxy, "Initial content");
            assert_truncate!(first_proxy, 0);
            assert_seek!(first_proxy, 0, Start);
            assert_write!(first_proxy, "As updated");

            let second_proxy = clone_get_vmo_file_proxy_assert_ok!(
                &first_proxy,
                fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE
            );

            assert_read!(second_proxy, "As updated");
            assert_truncate!(second_proxy, 0);
            assert_seek!(second_proxy, 0, Start);
            assert_write!(second_proxy, "Overwritten");

            assert_close!(first_proxy);
            assert_close!(second_proxy);
        },
    );
}

#[test]
fn get_attr_read_only() {
    run_server_client(fio::OpenFlags::RIGHT_READABLE, read_only(b"Content"), |proxy| async move {
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
    });
}

#[test]
fn get_attr_read_only_with_inode() {
    const INODE: u64 = 12345;
    const CONTENT: &'static [u8] = b"Content";
    let content_len: u64 = CONTENT.len().try_into().unwrap();
    let vmo = Vmo::create(content_len).unwrap();
    vmo.write(&CONTENT, 0).unwrap();

    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        VmoFile::new_with_inode(vmo, true, false, false, INODE),
        |proxy| async move {
            assert_get_attr!(
                proxy,
                fio::NodeAttributes {
                    mode: fio::MODE_TYPE_FILE | S_IRUSR,
                    id: INODE, // Custom inode was specified for this file.
                    content_size: content_len,
                    storage_size: content_len,
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
        read_write(b"Content"),
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
        read_only(b"Initial content"),
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

#[test]
fn clone_can_not_remove_node_reference() {
    run_server_client(fio::OpenFlags::NODE_REFERENCE, read_write(b""), |first_proxy| async move {
        let second_proxy = clone_as_file_assert_err!(
            &first_proxy,
            fio::OpenFlags::DESCRIBE | fio::OpenFlags::RIGHT_READABLE,
            Status::ACCESS_DENIED
        );
        assert_read_fidl_err_closed!(second_proxy);
        assert_write_fidl_err_closed!(second_proxy, "Write attempt");
        let third_proxy =
            clone_get_vmo_file_proxy_assert_err!(&first_proxy, fio::OpenFlags::DESCRIBE);
        assert_eq!(
            third_proxy.query().await.expect("query failed"),
            fio::NODE_PROTOCOL_NAME.as_bytes()
        );
        assert_close!(third_proxy);
        assert_close!(first_proxy);
    });
}

/// This test checks a somewhat non-trivial case. Two clients are connected to the same file, and
/// we want to make sure that they see the same content. The file content will be initially set by
/// the `init_vmo` callback.
///
/// A `coordinator` is used to control relative execution of the clients and the server. Clients
/// wait before they open the file, read the file content and then wait before reading the file
/// content once again.
#[test]
fn mock_directory_with_one_file_and_two_connections() {
    let exec = TestExecutor::new();
    let scope = ExecutionScope::new();

    let server = read_write(b"Initial");

    let create_client = move |initial_content: &'static str,
                              after_wait_content: &'static str,
                              update_with: &'static str| {
        let (proxy, server_end) =
            create_proxy::<fio::FileMarker>().expect("Failed to create connection endpoints");

        let (start_tx, start_rx) = oneshot::channel::<()>();
        let (write_and_close_tx, write_and_close_rx) = oneshot::channel::<()>();

        let server = server.clone();
        let scope = scope.clone();

        (
            move || async move {
                start_rx.await.unwrap();

                let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
                flags.to_object_request(server_end).handle(|object_request| {
                    vfs::file::serve(server, scope, &flags, object_request)
                });

                assert_read!(proxy, initial_content);

                write_and_close_rx.await.unwrap();

                assert_seek!(proxy, 0, Start);
                assert_read!(proxy, after_wait_content);

                assert_truncate!(proxy, 0);
                assert_seek!(proxy, 0, Start);
                assert_write!(proxy, update_with);

                assert_close!(proxy);
            },
            move || {
                start_tx.send(()).unwrap();
            },
            move || {
                write_and_close_tx.send(()).unwrap();
            },
        )
    };

    let (get_client1, client1_start, client1_write_and_close) =
        create_client("Initial", "Initial", "First update");
    let (get_client2, client2_start, client2_write_and_close) =
        create_client("Initial", "First update", "Second updated");

    test_client(|| async move {
        let client1 = get_client1();
        let client2 = get_client2();

        let _ = join(client1, client2).await;
    })
    .exec(exec)
    .coordinator(|mut controller| {
        client1_start();

        client2_start();

        controller.run_until_stalled();

        client1_write_and_close();

        controller.run_until_stalled();

        client2_write_and_close();
    })
    .run();
}

#[test]
fn get_vmo_read_only() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(b"Read only test"),
        |proxy| async move {
            {
                let vmo = assert_get_vmo!(proxy, fio::VmoFlags::READ);
                assert_vmo_content!(&vmo, b"Read only test");
            }

            {
                let vmo =
                    assert_get_vmo!(proxy, fio::VmoFlags::READ | fio::VmoFlags::SHARED_BUFFER);
                assert_vmo_content!(&vmo, b"Read only test");
            }

            {
                let vmo =
                    assert_get_vmo!(proxy, fio::VmoFlags::READ | fio::VmoFlags::PRIVATE_CLONE);
                assert_vmo_content!(&vmo, b"Read only test");
            }

            assert_read!(proxy, "Read only test");
            assert_close!(proxy);
        },
    );
}

#[test]
fn get_vmo_private_read_write() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Initial"),
        |proxy| {
            async move {
                {
                    let vmo = assert_get_vmo!(
                        proxy,
                        fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::PRIVATE_CLONE
                    );

                    assert_vmo_content!(&vmo, b"Initial");

                    // VMO can be updated only via the file interface.  There is no writable sharing
                    // via VMOs.
                    assert_write!(proxy, "No shared");

                    assert_vmo_content!(&vmo, b"Initial");

                    // Private clones can change properties and the VMO size.
                    vmo.set_content_size(&3).unwrap();
                    assert_vmo_content!(&vmo, b"Ini");
                    vmo.set_size(2).unwrap();
                    assert_vmo_content!(&vmo, b"In");
                    // The file is unaffected by these changes.
                    assert_seek!(proxy, 0, End, Ok(9));
                }
                {
                    let vmo = assert_get_vmo!(
                        proxy,
                        fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::PRIVATE_CLONE
                    );

                    assert_vmo_content!(&vmo, b"No shared");
                    vmo.write(b"-Private-", 0).unwrap();

                    // VMO can be updated only via the file interface.  There is no writable sharing
                    // via VMOs.
                    assert_write!(proxy, " writable VMOs");

                    assert_vmo_content!(&vmo, b"-Private-");
                }

                assert_close!(proxy);
            }
        },
    );
}

#[test]
fn get_vmo_shared_read_write() {
    use crate::test_utils::assertions::reexport::Status;
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Initial"),
        |proxy| async move {
            let vmo1 = assert_get_vmo!(
                proxy,
                fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::SHARED_BUFFER
            );
            let vmo2 = assert_get_vmo!(
                proxy,
                fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::SHARED_BUFFER
            );

            assert_vmo_content!(&vmo1, b"Initial");
            assert_vmo_content!(&vmo2, b"Initial");

            // Writes to one VMO can be observed in the file and in other shared VMOs.
            vmo1.write(b"Changed", 0).unwrap();
            assert_vmo_content!(&vmo1, b"Changed");
            assert_vmo_content!(&vmo2, b"Changed");
            assert_read!(proxy, "Changed");
            assert_seek!(proxy, 0, End, Ok(7));

            // Writing beyond the VMO's content size doesn't change the content size.
            vmo1.write(b"Beyond-content-size", 0).unwrap();
            assert_vmo_content!(&vmo1, b"Beyond-");
            assert_vmo_content!(&vmo2, b"Beyond-");
            assert_seek!(proxy, 0, End, Ok(7));

            // Doesn't have permission to change properties.
            assert_matches::assert_matches!(vmo1.set_content_size(&10), Err(Status::ACCESS_DENIED));
            // Doesn't have permission to change the vmo size.
            assert_matches::assert_matches!(vmo1.set_size(10), Err(Status::ACCESS_DENIED));

            assert_close!(proxy);
        },
    );
}

#[test]
fn get_vmo_two_vmos() {
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        read_write(b"Initial"),
        //                  0           0         1
        //                  0123456     012345678901234
        |proxy| async move {
            let vmo1 = assert_get_vmo!(proxy, fio::VmoFlags::READ | fio::VmoFlags::SHARED_BUFFER);

            assert_vmo_content!(&vmo1, b"Initial");

            assert_write!(proxy, "Updated content");

            let vmo2 = assert_get_vmo!(proxy, fio::VmoFlags::READ | fio::VmoFlags::SHARED_BUFFER);

            assert_vmo_content!(&vmo2, b"Updated content");
            assert_vmo_content!(&vmo1, b"Updated content");

            assert_close!(proxy);
        },
    );
}

#[test]
fn read_only_vmo_file() {
    let vmo = Vmo::create(1024).expect("create failed");
    let data = b"Read only str";
    vmo.write(data, 0).expect("write failed");
    vmo.set_content_size(&(data.len() as u64)).expect("set_content_size failed");
    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        VmoFile::new(
            vmo, /*readable*/ true, /*writable*/ false, /*executable*/ false,
        ),
        |proxy| async move {
            assert_read!(proxy, "Read only str");
            assert_close!(proxy);
        },
    );
}
