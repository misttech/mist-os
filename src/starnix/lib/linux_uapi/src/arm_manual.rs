// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

fn saturating_u64_to_u32(v: u64) -> u32 {
    if v > u32::max_value().into() {
        u32::max_value()
    } else {
        v as u32
    }
}

fn saturating_i64_to_i32(v: i64) -> i32 {
    if v > i32::max_value().into() {
        i32::max_value()
    } else if v < i32::min_value().into() {
        i32::min_value()
    } else {
        v as i32
    }
}

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

impl From<crate::timespec> for crate::arch32::timespec {
    fn from(tv: crate::timespec) -> Self {
        Self {
            tv_sec: saturating_i64_to_i32(tv.tv_sec),
            tv_nsec: saturating_i64_to_i32(tv.tv_nsec),
        }
    }
}

impl From<crate::timeval> for crate::arch32::timeval {
    fn from(tv: crate::timeval) -> Self {
        Self {
            tv_sec: saturating_i64_to_i32(tv.tv_sec),
            tv_usec: saturating_i64_to_i32(tv.tv_usec),
        }
    }
}

impl From<crate::arch32::rlimit> for crate::rlimit {
    fn from(rlimit: crate::arch32::rlimit) -> Self {
        Self { rlim_cur: rlimit.rlim_cur.into(), rlim_max: rlimit.rlim_max.into() }
    }
}

impl From<crate::rlimit> for crate::arch32::rlimit {
    fn from(rlimit: crate::rlimit) -> Self {
        Self {
            rlim_cur: saturating_u64_to_u32(rlimit.rlim_cur),
            rlim_max: saturating_u64_to_u32(rlimit.rlim_max),
        }
    }
}

impl From<crate::arch32::sigaltstack> for crate::sigaltstack {
    fn from(sigaltstack: crate::arch32::sigaltstack) -> Self {
        Self {
            ss_sp: sigaltstack.ss_sp.into(),
            ss_flags: sigaltstack.ss_flags.into(),
            ss_size: sigaltstack.ss_size.into(),
            __bindgen_padding_0: Default::default(),
        }
    }
}

impl TryFrom<crate::sigaltstack> for crate::arch32::sigaltstack {
    type Error = ();
    fn try_from(sigaltstack: crate::sigaltstack) -> Result<Self, ()> {
        Ok(Self {
            ss_sp: sigaltstack.ss_sp.try_into().map_err(|_| ())?,
            ss_flags: sigaltstack.ss_flags.try_into().map_err(|_| ())?,
            ss_size: sigaltstack.ss_size.try_into().map_err(|_| ())?,
        })
    }
}

impl From<crate::__kernel_fsid_t> for crate::arch32::__kernel_fsid_t {
    fn from(fsid: crate::__kernel_fsid_t) -> Self {
        Self { val: fsid.val }
    }
}

impl TryFrom<crate::statfs> for crate::arch32::statfs64 {
    type Error = ();
    fn try_from(statfs: crate::statfs) -> Result<Self, ()> {
        Ok(Self {
            f_type: statfs.f_type.try_into().map_err(|_| ())?,
            f_bsize: statfs.f_bsize.try_into().map_err(|_| ())?,
            f_blocks: statfs.f_blocks.try_into().map_err(|_| ())?,
            f_bfree: statfs.f_bfree.try_into().map_err(|_| ())?,
            f_bavail: statfs.f_bavail.try_into().map_err(|_| ())?,
            f_files: statfs.f_files.try_into().map_err(|_| ())?,
            f_ffree: statfs.f_ffree.try_into().map_err(|_| ())?,
            f_fsid: statfs.f_fsid.into(),
            f_namelen: statfs.f_namelen.try_into().map_err(|_| ())?,
            f_frsize: statfs.f_frsize.try_into().map_err(|_| ())?,
            f_flags: statfs.f_flags.try_into().map_err(|_| ())?,
            f_spare: Default::default(),
        })
    }
}

impl From<crate::arch32::__kernel_sigaction> for crate::__kernel_sigaction {
    fn from(sigaction: crate::arch32::__kernel_sigaction) -> Self {
        Self {
            sa_handler: sigaction.sa_handler.into(),
            sa_mask: crate::sigset_t { sig: [sigaction.sa_mask.into()] },
            sa_flags: sigaction.sa_flags.into(),
            sa_restorer: sigaction.sa_restorer.into(),
        }
    }
}

impl TryFrom<crate::__kernel_sigaction> for crate::arch32::__kernel_sigaction {
    type Error = ();
    fn try_from(sigaction: crate::__kernel_sigaction) -> Result<Self, ()> {
        Ok(Self {
            sa_handler: sigaction.sa_handler.try_into().map_err(|_| ())?,
            sa_mask: sigaction.sa_mask.sig[0].try_into().map_err(|_| ())?,
            sa_flags: sigaction.sa_flags.try_into().map_err(|_| ())?,
            sa_restorer: sigaction.sa_restorer.try_into().map_err(|_| ())?,
        })
    }
}
