// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

impl From<crate::stat> for crate::arch32::stat64 {
    fn from(stat: crate::stat) -> Self {
        let mut result = Self::default();
        // TODO(https://fxbug.dev/380431743): check conversions
        result.st_dev = stat.st_dev as u64;
        result.__st_ino = stat.st_ino as u32;
        result.st_mode = stat.st_mode;
        result.st_nlink = stat.st_nlink;
        result.st_uid = stat.st_uid as u32;
        result.st_gid = stat.st_gid as u32;
        result.st_rdev = stat.st_rdev;
        result.st_size = stat.st_size;
        result.st_blksize = stat.st_blksize as u32;
        result.st_blocks = stat.st_blocks as u64;
        result.st_atime = stat.st_atime as u32;
        result.st_atime_nsec = stat.st_atime_nsec as u32;
        result.st_mtime = stat.st_mtime as u32;
        result.st_mtime_nsec = stat.st_mtime_nsec as u32;
        result.st_ctime = stat.st_ctime as u32;
        result.st_ctime_nsec = stat.st_ctime_nsec as u32;
        result.st_ino = stat.st_ino;
        result
    }
}
