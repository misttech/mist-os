// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uapi;
use bitflags::bitflags;
use {fidl_fuchsia_io as fio, fidl_fuchsia_starnix_binder as fbinder};

// The inner mod is required because bitflags cannot pass the attribute through to the single
// variant, and attributes cannot be applied to macro invocations.
mod inner_flags {
    // Part of the code for the O_RDONLY case that's produced by the macro triggers the lint, but as
    // a whole, the produced code is still correct.
    #![allow(clippy::bad_bit_mask)] // TODO(b/303500202) Remove once addressed in bitflags.
    use super::{bitflags, uapi};

    bitflags! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct OpenFlags: u32 {
        const ACCESS_MASK = 0x3;

        // The access modes are not really bits. Instead, they're an enum
        // embedded in the bitfield. Use ACCESS_MASK to extract the enum
        // or use the OpenFlags::can_read and OpenFlags::can_write functions.
        const RDONLY = uapi::O_RDONLY;
        const WRONLY = uapi::O_WRONLY;
        const RDWR = uapi::O_RDWR;

        const CREAT = uapi::O_CREAT;
        const EXCL = uapi::O_EXCL;
        const NOCTTY = uapi::O_NOCTTY;
        const TRUNC = uapi::O_TRUNC;
        const APPEND = uapi::O_APPEND;
        const NONBLOCK = uapi::O_NONBLOCK;
        const DSYNC = uapi::O_DSYNC;
        const ASYNC = uapi::FASYNC;
        const DIRECT = uapi::O_DIRECT;
        const LARGEFILE = uapi::O_LARGEFILE;
        const DIRECTORY = uapi::O_DIRECTORY;
        const NOFOLLOW = uapi::O_NOFOLLOW;
        const NOATIME = uapi::O_NOATIME;
        const CLOEXEC = uapi::O_CLOEXEC;
        const SYNC = uapi::O_SYNC;
        const PATH = uapi::O_PATH;
        const TMPFILE = uapi::O_TMPFILE;
        const NDELAY = uapi::O_NDELAY;
        }
    }

    impl OpenFlags {
        pub fn can_read(&self) -> bool {
            let access_mode = self.bits() & Self::ACCESS_MASK.bits();
            access_mode == uapi::O_RDONLY || access_mode == uapi::O_RDWR
        }

        pub fn can_write(&self) -> bool {
            let access_mode = self.bits() & Self::ACCESS_MASK.bits();
            access_mode == uapi::O_WRONLY || access_mode == uapi::O_RDWR
        }
    }
}

pub use inner_flags::OpenFlags;

impl From<fio::OpenFlags> for OpenFlags {
    fn from(fio_flags: fio::OpenFlags) -> Self {
        let mut result = if fio_flags.contains(fio::OpenFlags::RIGHT_WRITABLE) {
            if fio_flags.contains(fio::OpenFlags::RIGHT_READABLE) {
                OpenFlags::RDWR
            } else {
                OpenFlags::WRONLY
            }
        } else {
            OpenFlags::RDONLY
        };
        if fio_flags.contains(fio::OpenFlags::CREATE) {
            result |= OpenFlags::CREAT;
        }
        if fio_flags.contains(fio::OpenFlags::CREATE_IF_ABSENT) {
            result |= OpenFlags::EXCL;
        }
        if fio_flags.contains(fio::OpenFlags::TRUNCATE) {
            result |= OpenFlags::TRUNC;
        }
        if fio_flags.contains(fio::OpenFlags::APPEND) {
            result |= OpenFlags::APPEND;
        }
        if fio_flags.contains(fio::OpenFlags::DIRECTORY) {
            result |= OpenFlags::DIRECTORY;
        }
        result
    }
}

impl From<OpenFlags> for fio::OpenFlags {
    fn from(flags: OpenFlags) -> fio::OpenFlags {
        let mut result = fio::OpenFlags::empty();
        if flags.can_read() {
            result |= fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE;
        }
        if flags.can_write() {
            result |= fio::OpenFlags::RIGHT_WRITABLE;
        }
        if flags.contains(OpenFlags::CREAT) {
            result |= fio::OpenFlags::CREATE;
        }
        if flags.contains(OpenFlags::EXCL) {
            result |= fio::OpenFlags::CREATE_IF_ABSENT;
        }
        if flags.contains(OpenFlags::TRUNC) {
            result |= fio::OpenFlags::TRUNCATE;
        }
        if flags.contains(OpenFlags::APPEND) {
            result |= fio::OpenFlags::APPEND;
        }
        if flags.contains(OpenFlags::DIRECTORY) {
            result |= fio::OpenFlags::DIRECTORY;
        }
        result
    }
}

impl From<OpenFlags> for fbinder::FileFlags {
    fn from(flags: OpenFlags) -> Self {
        let mut result = Self::empty();
        if flags.can_read() {
            result |= Self::RIGHT_READABLE;
        }
        if flags.can_write() {
            result |= Self::RIGHT_WRITABLE;
        }
        if flags.contains(OpenFlags::DIRECTORY) {
            result |= Self::DIRECTORY;
        }

        result
    }
}

impl From<fbinder::FileFlags> for OpenFlags {
    fn from(flags: fbinder::FileFlags) -> Self {
        let readable = flags.contains(fbinder::FileFlags::RIGHT_READABLE);
        let writable = flags.contains(fbinder::FileFlags::RIGHT_WRITABLE);
        let mut result = Self::empty();
        if readable && writable {
            result = Self::RDWR;
        } else if writable {
            result = Self::WRONLY;
        } else if readable {
            result = Self::RDONLY;
        }
        if flags.contains(fbinder::FileFlags::DIRECTORY) {
            result |= Self::DIRECTORY;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_flags_equals(flags: OpenFlags, fio_flags: fio::OpenFlags) {
        assert_eq!(flags, fio_flags.into());
        assert_eq!(fio_flags, flags.into());
    }

    #[::fuchsia::test]
    fn test_access() {
        let read_only = OpenFlags::from_bits_truncate(uapi::O_RDONLY);
        assert!(read_only.can_read());
        assert!(!read_only.can_write());

        let write_only = OpenFlags::from_bits_truncate(uapi::O_WRONLY);
        assert!(!write_only.can_read());
        assert!(write_only.can_write());

        let read_write = OpenFlags::from_bits_truncate(uapi::O_RDWR);
        assert!(read_write.can_read());
        assert!(read_write.can_write());
    }

    #[::fuchsia::test]
    fn test_conversion() {
        assert_flags_equals(
            OpenFlags::RDONLY,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE,
        );
        assert_flags_equals(OpenFlags::WRONLY, fio::OpenFlags::RIGHT_WRITABLE);
        assert_flags_equals(
            OpenFlags::RDWR,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::POSIX_WRITABLE,
        );
    }
}
