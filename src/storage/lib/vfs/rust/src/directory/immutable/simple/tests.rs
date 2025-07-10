// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the [`crate::directory::immutable::Simple`] directory.

use crate::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use crate::directory::helper::DirectlyMutable;
use crate::directory::immutable::Simple;
use crate::directory::serve;
use crate::directory::test_utils::DirentsSameInodeBuilder;
use crate::execution_scope::ExecutionScope;
use crate::file::{self, FidlIoConnection, File, FileIo, FileLike, FileOptions};
use crate::node::Node;
use crate::path::Path;
use crate::{
    assert_close, assert_get_attr, assert_query, assert_read, assert_read_dirents, assert_seek,
    assert_write,
};
use assert_matches::assert_matches;
use fidl_fuchsia_io as fio;
use fuchsia_fs::directory::{
    open_directory, open_directory_async, open_file, open_file_async, WatchEvent, WatchMessage,
    Watcher,
};
use fuchsia_sync::Mutex;
use futures::StreamExt as _;
use libc::{S_IFDIR, S_IRUSR, S_IXUSR};
use static_assertions::assert_eq_size;
use std::path::PathBuf;
use std::sync::Arc;
use vfs_macros::pseudo_directory;
use zx_status::Status;

async fn assert_open_file_err(
    root: &fio::DirectoryProxy,
    path: &str,
    flags: fio::Flags,
    expected_status: Status,
) {
    let file = open_file_async(&root, path, flags).unwrap();
    assert_matches!(
        file.take_event_stream().next().await,
        Some(Err(fidl::Error::ClientChannelClosed { status, .. })) if status == expected_status
    );
}

async fn assert_open_directory_err(
    root: &fio::DirectoryProxy,
    path: &str,
    flags: fio::Flags,
    expected_status: Status,
) {
    let file = open_directory_async(&root, path, flags).unwrap();
    assert_matches!(
        file.take_event_stream().next().await,
        Some(Err(fidl::Error::ClientChannelClosed { status, .. })) if status == expected_status
    );
}

#[fuchsia::test]
async fn empty_directory() {
    let dir = Simple::new();
    let root = serve(dir, fio::PERM_READABLE);
    assert_close!(root);
}

#[fuchsia::test]
async fn empty_directory_get_attr() {
    let dir = Simple::new();
    let root = serve(dir, fio::PERM_READABLE);
    assert_get_attr!(
        root,
        fio::NodeAttributes {
            mode: S_IFDIR | S_IRUSR | S_IXUSR,
            id: fio::INO_UNKNOWN,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        }
    );
    assert_close!(root);
}

#[fuchsia::test]
async fn empty_directory_with_custom_inode_get_attr() {
    let dir = Simple::new_with_inode(12345);
    let root = serve(dir, fio::PERM_READABLE);
    assert_get_attr!(
        root,
        fio::NodeAttributes {
            mode: S_IFDIR | S_IRUSR | S_IXUSR,
            id: 12345,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        }
    );
    assert_close!(root);
}

#[fuchsia::test]
async fn empty_directory_describe() {
    let dir = Simple::new();
    let root = serve(dir, fio::PERM_READABLE);
    assert_query!(root, fio::DIRECTORY_PROTOCOL_NAME);
    assert_close!(root);
}

#[fuchsia::test]
async fn open_empty_directory_with_describe() {
    let dir = Simple::new();
    let root = serve(dir, fio::PERM_READABLE | fio::Flags::FLAG_SEND_REPRESENTATION);
    assert_matches!(
        root.take_event_stream().next().await,
        Some(Ok(fio::DirectoryEvent::OnRepresentation { .. }))
    );
}

#[fuchsia::test]
async fn clone() {
    let dir = pseudo_directory! {
        "file" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);
    let file = open_file(&root, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "Content");
    assert_close!(file);

    let (root_clone, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    root.clone(server.into_channel().into()).unwrap();
    let file = open_file(&root_clone, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "Content");
    assert_close!(file);

    assert_close!(root);
    assert_close!(root_clone);
}

#[fuchsia::test]
async fn one_file_open_existing() {
    let dir = pseudo_directory! {
        "file" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    let file = open_file(&root, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "Content");
    assert_close!(file);

    assert_close!(root);
}

#[fuchsia::test]
async fn one_file_open_missing() {
    let dir = pseudo_directory! {
        "file" => file::read_only("Content"),
    };

    let root = serve(dir, fio::PERM_READABLE);
    assert_open_file_err(&root, "file2", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_close!(root);
}

#[fuchsia::test]
async fn one_file_open_missing_not_found_handler() {
    let dir = pseudo_directory! {
        "file" => file::read_only("Content"),
    };

    let last_handler_value = Arc::new(Mutex::new(None));
    let last_handler_value_clone = last_handler_value.clone();
    dir.clone().set_not_found_handler(Box::new(move |path| {
        *last_handler_value_clone.lock() = Some(path.to_string());
    }));

    let root = serve(dir, fio::PERM_READABLE);
    assert_open_file_err(&root, "file2", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_close!(root);
    assert_eq!(Some("file2".to_string()), *last_handler_value.lock());
}

#[fuchsia::test]
async fn small_tree_traversal() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            "fstab" => file::read_only(b"/dev/fs /"),
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
        "uname" => file::read_only(b"Fuchsia"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    async fn assert_contents(root: &fio::DirectoryProxy, path: &str, expected_contents: &str) {
        let file = open_file(&root, path, fio::PERM_READABLE).await.unwrap();
        assert_read!(file, expected_contents);
        assert_close!(file);
    }
    assert_contents(&root, "etc/fstab", "/dev/fs /").await;
    assert_contents(&root, "etc/ssh/sshd_config", "# Empty").await;
    assert_contents(&root, "uname", "Fuchsia").await;

    let ssh_dir = open_directory(&root, "etc/ssh", fio::PERM_READABLE).await.unwrap();
    assert_contents(&ssh_dir, "sshd_config", "# Empty").await;

    assert_close!(ssh_dir);
    assert_close!(root);
}

#[fuchsia::test]
async fn open_writable_in_subdir() {
    let dir = {
        pseudo_directory! {
            "etc" => pseudo_directory! {
                "ssh" => pseudo_directory! {
                    "sshd_config" => Arc::new(MockWritableFile),
                }
            }
        }
    };
    let root = serve(dir, fio::PERM_READABLE | fio::PERM_WRITABLE);
    let ssh_dir =
        open_directory(&root, "etc/ssh", fio::PERM_READABLE | fio::PERM_WRITABLE).await.unwrap();
    let file =
        open_file(&ssh_dir, "sshd_config", fio::PERM_READABLE | fio::PERM_WRITABLE).await.unwrap();
    assert_read!(file, MOCK_FILE_CONTENTS);
    assert_seek!(file, 0, Start);
    assert_write!(file, "new content");
    assert_close!(file);
}

#[fuchsia::test]
async fn open_non_existing_path() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "file1" => file::read_only(b"Content 1"),
        },
        "file2" => file::read_only(b"Content 2"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "non-existing", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_open_file_err(&root, "dir/file10", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_open_file_err(&root, "dir/dir/file10", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_open_file_err(&root, "dir/dir/file1", fio::PERM_READABLE, Status::NOT_FOUND).await;
    assert_close!(root);
}

#[fuchsia::test]
async fn open_empty_path() {
    let dir = pseudo_directory! {
        "file_foo" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);
    assert_open_file_err(&root, "", fio::PERM_READABLE, Status::INVALID_ARGS).await;
    assert_close!(root);
}

#[fuchsia::test]
async fn open_path_within_a_file() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "file1" => file::read_only(b"Content 1"),
        },
        "file2" => file::read_only(b"Content 2"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "file2/file1", fio::PERM_READABLE, Status::NOT_DIR).await;
    assert_open_file_err(&root, "dir/file1/file3", fio::PERM_READABLE, Status::NOT_DIR).await;

    assert_close!(root);
}

#[fuchsia::test]
async fn open_file_as_directory() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "file1" => file::read_only(b"Content 1"),
        },
        "file2" => file::read_only(b"Content 2"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_directory_err(&root, "file2", fio::PERM_READABLE, Status::NOT_DIR).await;
    assert_open_directory_err(&root, "dir/file1", fio::PERM_READABLE, Status::NOT_DIR).await;

    assert_close!(root);
}

#[fuchsia::test]
async fn open_directory_as_file() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "dir2" => pseudo_directory! {},
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "dir", fio::PERM_READABLE, Status::NOT_FILE).await;
    assert_open_file_err(&root, "dir/dir2", fio::PERM_READABLE, Status::NOT_FILE).await;

    assert_close!(root);
}

#[fuchsia::test]
// TODO(https://fxbug.dev/405151790): open3 doesn't enforce the trailing slash meaning directory.
// Either enable this test when it does or delete/modify it if we decide on a different policy.
#[ignore]
async fn trailing_slash_means_directory() {
    let dir = pseudo_directory! {
        "file" => file::read_only(b"Content"),
        "dir" => pseudo_directory! {},
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "file/", fio::PERM_READABLE, Status::NOT_DIR).await;

    let file = open_file(&root, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "Content");
    assert_close!(file);

    let sub_dir = open_directory(&root, "dir/", fio::PERM_READABLE).await.unwrap();
    assert_close!(sub_dir);

    assert_close!(root);
}

#[fuchsia::test]
async fn no_dots_in_open() {
    let dir = pseudo_directory! {
        "file" => file::read_only(b"Content"),
        "dir" => pseudo_directory! {
            "dir2" => pseudo_directory! {},
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_directory_err(&root, "dir/../dir2", fio::PERM_READABLE, Status::INVALID_ARGS).await;
    assert_open_directory_err(&root, "dir/./dir2", fio::PERM_READABLE, Status::INVALID_ARGS).await;
    assert_open_directory_err(&root, "./dir2", fio::PERM_READABLE, Status::INVALID_ARGS).await;

    assert_close!(root);
}

#[fuchsia::test]
async fn no_consecutive_slashes_in_open() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "dir2" => pseudo_directory! {},
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_directory_err(&root, "dir//dir2", fio::PERM_READABLE, Status::INVALID_ARGS).await;
    assert_open_directory_err(&root, "dir/dir2//", fio::PERM_READABLE, Status::INVALID_ARGS).await;
    assert_open_directory_err(&root, "//dir/dir2", fio::PERM_READABLE, Status::INVALID_ARGS).await;

    assert_close!(root);
}

#[fuchsia::test]
async fn directories_restrict_nested_read_permissions() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "file" => file::read_only(b"Content"),
        },
    };
    let root = serve(dir, fio::Flags::empty());
    assert_open_file_err(&root, "dir/file", fio::PERM_READABLE, Status::ACCESS_DENIED).await;
    assert_close!(root);
}

#[fuchsia::test]
async fn directories_restrict_nested_write_permissions() {
    let dir = pseudo_directory! {
        "dir" => pseudo_directory! {
            "file" => Arc::new(MockWritableFile),
        },
    };
    let root = serve(dir, fio::Flags::empty());
    assert_open_file_err(&root, "dir/file", fio::PERM_WRITABLE, Status::ACCESS_DENIED).await;
    assert_close!(root);
}

#[fuchsia::test]
async fn directories_remove_nested() {
    // Test dynamic removal of a subdirectory under another directory.
    let root = pseudo_directory! {
        "dir" => pseudo_directory! {
            "subdir" => pseudo_directory! {},   // To be removed below.
        },
    };
    let dir_entry = root.get_entry("dir").expect("Failed to get directory entry!");
    // Remove subdir from dir.
    let downcasted_dir = dir_entry.into_any().downcast::<Simple>().expect("Downcast failed!");
    downcasted_dir.remove_entry("subdir", true).expect("Failed to remove directory entry!");

    // Ensure it was actually removed.
    assert_eq!(downcasted_dir.get_entry("subdir").err(), Some(Status::NOT_FOUND));
}

#[fuchsia::test]
async fn flag_inherit_write_means_writable() {
    let dir = {
        pseudo_directory! {
        "nested" => pseudo_directory! {
            "file" => Arc::new(MockWritableFile),
            }
        }
    };
    let root = serve(dir, fio::PERM_READABLE | fio::PERM_WRITABLE);
    let sub_dir =
        open_directory(&root, "nested", fio::PERM_READABLE | fio::Flags::PERM_INHERIT_WRITE)
            .await
            .unwrap();
    let file = open_file(&sub_dir, "file", fio::PERM_READABLE | fio::PERM_WRITABLE).await.unwrap();

    assert_read!(file, MOCK_FILE_CONTENTS);
    assert_seek!(file, 0, Start);
    assert_write!(file, "new content");

    assert_close!(file);
    assert_close!(sub_dir);
    assert_close!(root);
}

#[fuchsia::test]
async fn flag_inherit_write_does_not_add_writable_to_read_only() {
    let dir = pseudo_directory! {
        "nested" => pseudo_directory! {
            "file" => Arc::new(MockWritableFile),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);
    let sub_dir =
        open_directory(&root, "nested", fio::PERM_READABLE | fio::Flags::PERM_INHERIT_WRITE)
            .await
            .unwrap();
    assert_open_file_err(
        &root,
        "file",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
        Status::ACCESS_DENIED,
    )
    .await;

    let file = open_file(&sub_dir, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, MOCK_FILE_CONTENTS);

    assert_close!(file);
    assert_close!(sub_dir);
    assert_close!(root);
}

#[fuchsia::test]
async fn read_dirents_large_buffer() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            "fstab" => file::read_only(b"/dev/fs /"),
            "passwd" => file::read_only(b"[redacted]"),
            "shells" => file::read_only(b"/bin/bash"),
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
        "files" => file::read_only(b"Content"),
        "more" => file::read_only(b"Content"),
        "uname" => file::read_only(b"Fuchsia"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected
        .add(fio::DirentType::Directory, b".")
        .add(fio::DirentType::Directory, b"etc")
        .add(fio::DirentType::File, b"files")
        .add(fio::DirentType::File, b"more")
        .add(fio::DirentType::File, b"uname");
    assert_read_dirents!(root, 1000, expected.into_vec());

    let etc_dir = open_directory(&root, "etc", fio::PERM_READABLE).await.unwrap();
    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected
        .add(fio::DirentType::Directory, b".")
        .add(fio::DirentType::File, b"fstab")
        .add(fio::DirentType::File, b"passwd")
        .add(fio::DirentType::File, b"shells")
        .add(fio::DirentType::Directory, b"ssh");
    assert_read_dirents!(etc_dir, 1000, expected.into_vec());
    assert_close!(etc_dir);

    let ssh_dir = open_directory(&root, "etc/ssh", fio::PERM_READABLE).await.unwrap();
    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected.add(fio::DirentType::Directory, b".").add(fio::DirentType::File, b"sshd_config");
    assert_read_dirents!(ssh_dir, 1000, expected.into_vec());
    assert_close!(ssh_dir);

    assert_close!(root);
}

#[fuchsia::test]
async fn read_dirents_small_buffer() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! { },
        "files" => file::read_only(b"Content"),
        "more" => file::read_only(b"Content"),
        "uname" => file::read_only(b"Fuchsia"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    // Entry header is 10 bytes + length of the name in bytes.
    // (10 + 1) = 11
    expected.add(fio::DirentType::Directory, b".");
    assert_read_dirents!(root, 11, expected.into_vec());

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected
        // (10 + 3) = 13
        .add(fio::DirentType::Directory, b"etc")
        // 13 + (10 + 5) = 28
        .add(fio::DirentType::File, b"files");
    assert_read_dirents!(root, 28, expected.into_vec());

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected.add(fio::DirentType::File, b"more").add(fio::DirentType::File, b"uname");
    assert_read_dirents!(root, 100, expected.into_vec());

    assert_read_dirents!(root, 100, vec![]);
    assert_close!(root);
}

#[fuchsia::test]
async fn read_dirents_very_small_buffer() {
    let dir = pseudo_directory! {
        "file" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);
    let (status, entries) = root.read_dirents(8).await.expect("read_dirents fidl error");
    assert_eq!(Status::from_raw(status), Status::BUFFER_TOO_SMALL);
    assert_eq!(entries.len(), 0);
    assert_close!(root);
}

#[fuchsia::test]
async fn read_dirents_rewind() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! { },
        "files" => file::read_only(b"Content"),
        "more" => file::read_only(b"Content"),
        "uname" => file::read_only(b"Fuchsia"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    // Entry header is 10 bytes + length of the name in bytes.
    expected
        // (10 + 1) = 11
        .add(fio::DirentType::Directory, b".")
        // 11 + (10 + 3) = 24
        .add(fio::DirentType::Directory, b"etc")
        // 24 + (10 + 5) = 39
        .add(fio::DirentType::File, b"files");
    assert_read_dirents!(root, 39, expected.into_vec());

    let status = root.rewind().await.expect("rewind fidl error");
    assert_eq!(Status::from_raw(status), Status::OK);

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    // Entry header is 10 bytes + length of the name in bytes.
    expected
        // (10 + 1) = 11
        .add(fio::DirentType::Directory, b".")
        // 11 + (10 + 3) = 24
        .add(fio::DirentType::Directory, b"etc");
    assert_read_dirents!(root, 24, expected.into_vec());

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected
        .add(fio::DirentType::File, b"files")
        .add(fio::DirentType::File, b"more")
        .add(fio::DirentType::File, b"uname");
    assert_read_dirents!(root, 200, expected.into_vec());

    assert_read_dirents!(root, 100, vec![]);
    assert_close!(root);
}

#[fuchsia::test]
async fn add_entry_too_long_error() {
    assert_eq_size!(u64, usize);

    // It is annoying to have to write `as u64` or `as usize` everywhere.  Converting
    // `MAX_FILENAME` to `usize` aligns the types.
    let max_filename = fio::MAX_NAME_LENGTH as usize;

    let dir = Simple::new();
    let name = {
        let mut name = "This entry name will be longer than the MAX_FILENAME bytes".to_string();

        // Make `name` at least `MAX_FILENAME + 1` bytes long.
        name.reserve(max_filename + 1);
        let filler = " - filler";
        name.push_str(&filler.repeat((max_filename + filler.len()) / filler.len()));

        // And we want exactly `MAX_FILENAME + 1` bytes.  As all the characters are ASCII, we
        // should be able to just cut at any byte.
        name.truncate(max_filename + 1);
        assert!(name.len() == max_filename + 1);

        name
    };
    let name_len = name.len();

    match dir.clone().add_entry(name, file::read_only(b"Should never be used")) {
        Ok(()) => panic!(
            "`add_entry()` succeeded for a name of {} bytes, when MAX_FILENAME is {}",
            name_len, max_filename
        ),
        Err(Status::BAD_PATH) => (),
        Err(status) => panic!(
            "`add_entry()` failed for a name of {} bytes, with status {}.  Expected status is \
             BAD_PATH.  MAX_FILENAME is {}.",
            name_len, status, max_filename
        ),
    }

    // Make sure that after we have seen an error, the entry is not actually inserted.

    let root = serve(dir, fio::PERM_READABLE);
    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected.add(fio::DirentType::Directory, b".");
    assert_read_dirents!(root, 1000, expected.into_vec());
    assert_close!(root);
}

#[fuchsia::test]
async fn simple_add_file() {
    let dir = Simple::new();
    let root = serve(dir.clone(), fio::PERM_READABLE);

    assert_open_file_err(&root, "file", fio::PERM_READABLE, Status::NOT_FOUND).await;

    let file = file::read_only(b"Content");
    dir.add_entry("file", file).unwrap();

    let proxy = open_file(&root, "file", fio::PERM_READABLE).await.unwrap();
    assert_read!(proxy, "Content");
    assert_close!(proxy);
}

#[fuchsia::test]
async fn add_file_to_empty() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc -> /* empty */
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "etc/fstab", fio::PERM_READABLE, Status::NOT_FOUND).await;

    let fstab = file::read_only(b"/dev/fs /");
    etc.add_entry("fstab", fstab).unwrap();

    let proxy = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(proxy, "/dev/fs /");
    assert_close!(proxy);
}

#[fuchsia::test]
async fn in_tree_open() {
    let ssh;
    let _root = pseudo_directory! {
        "etc" => pseudo_directory! {
            "ssh" => pseudo_directory! {
                ssh ->
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
    };

    let ssh_dir = serve(ssh, fio::PERM_READABLE);
    let file = open_file(&ssh_dir, "sshd_config", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "# Empty");
    assert_close!(file);
    assert_close!(ssh_dir);
}

#[fuchsia::test]
async fn in_tree_open_path_one_component() {
    let etc;
    let _root = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
    };

    let path = Path::validate_and_split("ssh").unwrap();
    let ssh_dir = crate::serve_directory(etc, path, fio::PERM_READABLE);
    let file = open_file(&ssh_dir, "sshd_config", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "# Empty");
    assert_close!(file);
    assert_close!(ssh_dir);
}

#[fuchsia::test]
async fn in_tree_open_path_two_components() {
    let etc;
    let _root = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
    };

    let path = Path::validate_and_split("ssh/sshd_config").unwrap();
    let file = crate::serve_file(etc, path, fio::PERM_READABLE);
    assert_read!(file, "# Empty");
    assert_close!(file);
}

#[fuchsia::test]
async fn in_tree_add_file() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
            "passwd" => file::read_only(b"[redacted]"),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "etc/fstab", fio::PERM_READABLE, Status::NOT_FOUND).await;
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    let fstab = file::read_only(b"/dev/fs /");
    etc.add_entry("fstab", fstab).unwrap();

    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    assert_close!(root);
}

#[fuchsia::test]
async fn in_tree_remove_file() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "fstab" => file::read_only(b"/dev/fs /"),
            "passwd" => file::read_only(b"[redacted]"),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    let o_passwd = etc.remove_entry("passwd", false).unwrap();
    match o_passwd {
        None => panic!("remove_entry() did not find 'passwd'"),
        Some(passwd) => {
            let entry_info = passwd.entry_info();
            assert_eq!(entry_info, EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File));
        }
    }

    assert_open_file_err(&root, "etc/passwd", fio::PERM_READABLE, Status::NOT_FOUND).await;
    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);

    assert_close!(root);
}

#[fuchsia::test]
async fn in_tree_move_file() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "fstab" => file::read_only(b"/dev/fs /"),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "etc/passwd", fio::PERM_READABLE, Status::NOT_FOUND).await;
    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);

    let fstab = etc
        .clone()
        .remove_entry("fstab", false)
        .unwrap()
        .expect("remove_entry() did not find 'fstab'");

    etc.add_entry("passwd", fstab).unwrap();

    assert_open_file_err(&root, "etc/fstab", fio::PERM_READABLE, Status::NOT_FOUND).await;
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);

    assert_close!(root);
}

#[fuchsia::test]
async fn watch_empty() {
    let dir = Simple::new();
    let root = serve(dir, fio::PERM_READABLE);
    let mut watcher = Watcher::new(&root).await.unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    assert_close!(root);
}

#[fuchsia::test]
async fn watch_non_empty() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            "fstab" => file::read_only(b"/dev/fs /"),
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
        "files" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);
    let mut watcher = Watcher::new(&root).await.unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("etc") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("files") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    assert_close!(root);
}

#[fuchsia::test]
async fn watch_two_watchers() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            "fstab" => file::read_only(b"/dev/fs /"),
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
        "files" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);
    let mut watcher = Watcher::new(&root).await.unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("etc") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("files") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    let mut watcher2 = Watcher::new(&root).await.unwrap();

    assert_eq!(
        watcher2.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher2.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("etc") }))
    );
    assert_eq!(
        watcher2.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("files") }))
    );
    assert_eq!(
        watcher2.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    assert_close!(root);
}

#[fuchsia::test]
async fn watch_addition() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
            "passwd" => file::read_only(b"[redacted]"),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    assert_open_file_err(&root, "etc/fstab", fio::PERM_READABLE, Status::NOT_FOUND).await;
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    let etc_proxy = open_directory(&root, "etc", fio::PERM_READABLE).await.unwrap();
    let mut watcher = Watcher::new(&etc_proxy).await.unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("passwd") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("ssh") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    let fstab = file::read_only(b"/dev/fs /");
    etc.add_entry("fstab", fstab).unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("fstab") }))
    );

    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    assert_close!(etc_proxy);
    assert_close!(root);
}

#[fuchsia::test]
async fn watch_removal() {
    let etc;
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            etc ->
            "fstab" => file::read_only(b"/dev/fs /"),
            "passwd" => file::read_only(b"[redacted]"),
        },
    };
    let root = serve(dir, fio::PERM_READABLE);

    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);
    let file = open_file(&root, "etc/passwd", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "[redacted]");
    assert_close!(file);

    let etc_proxy = open_directory(&root, "etc", fio::PERM_READABLE).await.unwrap();
    let mut watcher = Watcher::new(&etc_proxy).await.unwrap();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("fstab") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("passwd") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    let o_passwd = etc.remove_entry("passwd", false).unwrap();
    match o_passwd {
        None => panic!("remove_entry() did not find 'passwd'"),
        Some(passwd) => {
            let entry_info = passwd.entry_info();
            assert_eq!(entry_info, EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File));
        }
    }

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage {
            event: WatchEvent::REMOVE_FILE,
            filename: PathBuf::from("passwd")
        }))
    );

    let file = open_file(&root, "etc/fstab", fio::PERM_READABLE).await.unwrap();
    assert_read!(file, "/dev/fs /");
    assert_close!(file);
    assert_open_file_err(&root, "etc/passwd", fio::PERM_READABLE, Status::NOT_FOUND).await;

    assert_close!(etc_proxy);
    assert_close!(root);
}

#[fuchsia::test]
async fn watch_with_mask() {
    let dir = pseudo_directory! {
        "etc" => pseudo_directory! {
            "fstab" => file::read_only(b"/dev/fs /"),
            "ssh" => pseudo_directory! {
                "sshd_config" => file::read_only(b"# Empty"),
            },
        },
        "files" => file::read_only(b"Content"),
    };
    let root = serve(dir, fio::PERM_READABLE);

    let mask = fio::WatchMask::IDLE | fio::WatchMask::ADDED | fio::WatchMask::REMOVED;
    let mut watcher = Watcher::new_with_mask(&root, mask).await.unwrap();
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() }))
    );

    assert_close!(root);
}

#[fuchsia::test]
async fn watch_remove_all_entries() {
    let dir = pseudo_directory! {
        "file1" => file::read_only(""),
        "file2" => file::read_only(""),
    };
    let root = serve(dir.clone(), fio::PERM_READABLE);
    let mut watcher = Watcher::new_with_mask(&root, fio::WatchMask::REMOVED).await.unwrap();

    dir.remove_all_entries();

    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("file1") }))
    );
    assert_eq!(
        watcher.next().await,
        Some(Ok(WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("file2") }))
    );

    assert_close!(root);
}

#[fuchsia::test]
async fn open_directory_containing_itself() {
    let dir = pseudo_directory! {};
    dir.add_entry("dir", dir.clone()).unwrap();

    let root = serve(dir.clone(), fio::PERM_READABLE);
    let sub_dir = open_directory(&root, "dir/dir/dir/dir", fio::PERM_READABLE).await.unwrap();

    assert_close!(sub_dir);
    assert_close!(root);

    dir.remove_entry("dir", true).unwrap();
}

struct MockWritableFile;
const MOCK_FILE_CONTENTS: &str = "mock-file-contents";

impl GetEntryInfo for MockWritableFile {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

impl DirectoryEntry for MockWritableFile {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_file(self)
    }
}

impl Node for MockWritableFile {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: 0,
                storage_size: 0,
                link_count: 1,
                id: fio::INO_UNKNOWN,
            }
        ))
    }
}

impl FileLike for MockWritableFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: crate::ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        FidlIoConnection::create_sync(scope, self, options, object_request.take());
        Ok(())
    }
}

impl File for MockWritableFile {
    fn writable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, _: u64) -> Result<(), Status> {
        unimplemented!()
    }

    async fn get_size(&self) -> Result<u64, Status> {
        unimplemented!()
    }

    async fn update_attributes(&self, _: fio::MutableNodeAttributes) -> Result<(), Status> {
        unimplemented!()
    }

    async fn sync(&self, _: file::SyncMode) -> Result<(), Status> {
        Ok(())
    }
}

impl FileIo for MockWritableFile {
    async fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<u64, Status> {
        assert_eq!(offset, 0);
        assert!(bytes.len() >= MOCK_FILE_CONTENTS.len());
        bytes[..MOCK_FILE_CONTENTS.len()].copy_from_slice(MOCK_FILE_CONTENTS.as_bytes());
        Ok(MOCK_FILE_CONTENTS.len() as u64)
    }

    async fn write_at(&self, _: u64, bytes: &[u8]) -> Result<u64, Status> {
        Ok(bytes.len() as u64)
    }

    async fn append(&self, _: &[u8]) -> Result<(u64, u64), Status> {
        unimplemented!()
    }
}
