// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the asynchronous files.

use super::{read_only, VmoFile};
use crate::execution_scope::ExecutionScope;
use crate::file::test_utils::*;
use crate::{
    assert_close, assert_event, assert_get_attr, assert_get_vmo, assert_get_vmo_err, assert_read,
    assert_read_at, assert_read_err, assert_read_fidl_err_closed, assert_seek, assert_truncate_err,
    assert_vmo_content, assert_write_err, assert_write_fidl_err_closed, clone_as_file_assert_err,
    clone_get_proxy_assert, clone_get_vmo_file_proxy_assert_err,
    clone_get_vmo_file_proxy_assert_ok, ToObjectRequest,
};
use assert_matches::assert_matches;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use fuchsia_async::TestExecutor;
use futures::channel::oneshot;
use futures::StreamExt;
use libc::S_IRUSR;
use zx::sys::ZX_OK;
use zx::Vmo;
use zx_status::Status;

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
        let (proxy, server_end) = create_proxy::<fio::FileMarker>();

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
fn read_only_write_is_not_supported() {
    let exec = TestExecutor::new();
    let scope = ExecutionScope::new();

    let server = read_only(b"Read only test");

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
    run_server_client(fio::OpenFlags::RIGHT_READABLE, read_only(b"Content"), |proxy| async move {
        assert_seek!(proxy, 20, Start);
    });
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
        fio::OpenFlags::RIGHT_READABLE,
        read_only(b"Initial content"),
        |first_proxy| async move {
            assert_read!(first_proxy, "Initial content");
            assert_seek!(first_proxy, 0, Start);

            let second_proxy =
                clone_get_vmo_file_proxy_assert_ok!(&first_proxy, fio::OpenFlags::DESCRIBE);

            assert_read_err!(second_proxy, Status::BAD_HANDLE);

            assert_close!(first_proxy);
        },
    );
}

#[test]
fn clone_inherit_access() {
    use fidl_fuchsia_io as fio;

    run_server_client(
        fio::OpenFlags::RIGHT_READABLE,
        read_only(b"Initial content"),
        |first_proxy| async move {
            assert_read!(first_proxy, "Initial content");

            let second_proxy = clone_get_vmo_file_proxy_assert_ok!(
                &first_proxy,
                fio::OpenFlags::CLONE_SAME_RIGHTS | fio::OpenFlags::DESCRIBE
            );

            assert_read!(second_proxy, "Initial content");

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
    run_server_client(fio::OpenFlags::NODE_REFERENCE, read_only(b""), |first_proxy| async move {
        let second_proxy = clone_as_file_assert_err!(
            &first_proxy,
            fio::OpenFlags::DESCRIBE | fio::OpenFlags::RIGHT_READABLE,
            Status::ACCESS_DENIED
        );
        assert_read_fidl_err_closed!(second_proxy);
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

            assert_get_vmo_err!(
                proxy,
                fio::VmoFlags::READ | fio::VmoFlags::WRITE,
                Status::ACCESS_DENIED
            );
            assert_get_vmo_err!(
                proxy,
                fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::SHARED_BUFFER,
                Status::ACCESS_DENIED
            );
            assert_get_vmo_err!(
                proxy,
                fio::VmoFlags::READ | fio::VmoFlags::WRITE | fio::VmoFlags::PRIVATE_CLONE,
                Status::ACCESS_DENIED
            );

            assert_read!(proxy, "Read only test");
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
