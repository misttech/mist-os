// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common utilities used by several directory implementations.

use crate::common::stricter_or_same_rights;
use crate::directory::entry::EntryInfo;

use byteorder::{LittleEndian, WriteBytesExt as _};
use fidl_fuchsia_io as fio;
use static_assertions::assert_eq_size;
use std::io::Write as _;
use std::mem::size_of;
use zx_status::Status;

/// Directories need to make sure that connections to child entries do not receive more rights than
/// the connection to the directory itself.  Plus there is special handling of the OPEN_FLAG_POSIX_*
/// flags. This function should be called before calling [`new_connection_validate_flags`] if both
/// are needed.
pub(crate) fn check_child_connection_flags(
    parent_flags: fio::OpenFlags,
    mut flags: fio::OpenFlags,
) -> Result<fio::OpenFlags, Status> {
    if flags & (fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::DIRECTORY)
        == fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::DIRECTORY
    {
        return Err(Status::INVALID_ARGS);
    }

    // Can only specify OPEN_FLAG_CREATE_IF_ABSENT if OPEN_FLAG_CREATE is also specified.
    if flags.intersects(fio::OpenFlags::CREATE_IF_ABSENT)
        && !flags.intersects(fio::OpenFlags::CREATE)
    {
        return Err(Status::INVALID_ARGS);
    }

    // Can only use CLONE_FLAG_SAME_RIGHTS when calling Clone.
    if flags.intersects(fio::OpenFlags::CLONE_SAME_RIGHTS) {
        return Err(Status::INVALID_ARGS);
    }

    // Remove POSIX flags when the respective rights are not available ("soft fail").
    if !parent_flags.intersects(fio::OpenFlags::RIGHT_EXECUTABLE) {
        flags &= !fio::OpenFlags::POSIX_EXECUTABLE;
    }
    if !parent_flags.intersects(fio::OpenFlags::RIGHT_WRITABLE) {
        flags &= !fio::OpenFlags::POSIX_WRITABLE;
    }

    // Can only use CREATE flags if the parent connection is writable.
    if flags.intersects(fio::OpenFlags::CREATE)
        && !parent_flags.intersects(fio::OpenFlags::RIGHT_WRITABLE)
    {
        return Err(Status::ACCESS_DENIED);
    }

    if stricter_or_same_rights(parent_flags, flags) {
        Ok(flags)
    } else {
        Err(Status::ACCESS_DENIED)
    }
}

/// A helper to generate binary encodings for the ReadDirents response.  This function will append
/// an entry description as specified by `entry` and `name` to the `buf`, and would return `true`.
/// In case this would cause the buffer size to exceed `max_bytes`, the buffer is then left
/// untouched and a `false` value is returned.
pub(crate) fn encode_dirent(
    buf: &mut Vec<u8>,
    max_bytes: u64,
    entry: &EntryInfo,
    name: &str,
) -> bool {
    let header_size = size_of::<u64>() + size_of::<u8>() + size_of::<u8>();

    assert_eq_size!(u64, usize);

    if buf.len() + header_size + name.len() > max_bytes as usize {
        return false;
    }

    assert!(
        name.len() <= fio::MAX_NAME_LENGTH as usize,
        "Entry names are expected to be no longer than MAX_FILENAME ({}) bytes.\n\
         Got entry: '{}'\n\
         Length: {} bytes",
        fio::MAX_NAME_LENGTH,
        name,
        name.len()
    );

    assert!(
        fio::MAX_NAME_LENGTH <= u8::max_value() as u64,
        "Expecting to be able to store MAX_FILENAME ({}) in one byte.",
        fio::MAX_NAME_LENGTH
    );

    buf.write_u64::<LittleEndian>(entry.inode())
        .expect("out should be an in memory buffer that grows as needed");
    buf.write_u8(name.len() as u8).expect("out should be an in memory buffer that grows as needed");
    buf.write_u8(entry.type_().into_primitive())
        .expect("out should be an in memory buffer that grows as needed");
    buf.write_all(name.as_ref()).expect("out should be an in memory buffer that grows as needed");

    true
}
