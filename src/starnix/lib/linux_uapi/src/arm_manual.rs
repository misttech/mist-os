// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{arch_translate_data, translate_data};

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

arch_translate_data! {
    BidiFrom<flock> {
        l_type,
        l_whence,
        l_start,
        l_len,
        l_pid
    }

    BidiFrom<itimerspec> {
        it_interval,
        it_value
    }

    BidiFrom<itimerval> {
        it_interval,
        it_value
    }

    BidiFrom<__kernel_fsid_t> {
        val
    }

    BidiFrom<sigaltstack> {
        ss_sp,
        ss_flags,
        ss_size
    }

    BidiFrom<cmsghdr> {
        cmsg_len,
        cmsg_level,
        cmsg_type,
    }

    BidiFrom<sock_filter> {
        code,
        jt,
        jf,
        k,
    }

    BidiFrom<ucred> {
        pid,
        uid,
        gid,
    }

    BidiFrom<rusage> {
        ru_utime,
        ru_stime,
        ru_maxrss,
        ru_ixrss,
        ru_idrss,
        ru_isrss,
        ru_minflt,
        ru_majflt,
        ru_nswap,
        ru_inblock,
        ru_oublock,
        ru_msgsnd,
        ru_msgrcv,
        ru_nsignals,
        ru_nvcsw,
        ru_nivcsw,
    }
}

translate_data! {
    TryFrom<crate::stat> for crate::arch32::stat64 {
        st_dev = st_dev;
        __st_ino = st_ino(0);
        st_mode = st_mode;
        st_nlink = st_nlink;
        st_uid = st_uid;
        st_gid = st_gid;
        st_rdev = st_rdev;
        st_size = st_size;
        st_blksize = st_blksize;
        st_blocks = st_blocks;
        st_atime = st_atime;
        st_atime_nsec = st_atime_nsec;
        st_mtime = st_mtime;
        st_mtime_nsec = st_mtime_nsec;
        st_ctime = st_ctime;
        st_ctime_nsec = st_ctime_nsec;
        st_ino = st_ino;
        ..Default::default()
    }

    TryFrom<crate::statfs> for crate::arch32::statfs64 {
        f_type = f_type;
        f_bsize = f_bsize;
        f_blocks = f_blocks;
        f_bfree = f_bfree;
        f_bavail = f_bavail;
        f_files = f_files;
        f_ffree = f_ffree;
        f_fsid = f_fsid;
        f_namelen = f_namelen;
        f_frsize = f_frsize;
        f_flags = f_flags;
        ..Default::default()
    }
}

arch_translate_data! {
    From32<timespec> {
        tv_sec,
        tv_nsec,
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

arch_translate_data! {
    From32<timeval> {
        tv_sec,
        tv_usec,
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

arch_translate_data! {
    From32<rlimit> {
        rlim_cur,
        rlim_max,
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

impl TryFrom<crate::sigset_t> for u32 {
    type Error = ();
    fn try_from(sigset: crate::sigset_t) -> Result<Self, ()> {
        sigset.sig[0].try_into().map_err(|_| ())
    }
}

impl From<u32> for crate::sigset_t {
    fn from(sigset: u32) -> Self {
        crate::sigset_t { sig: [sigset.into()] }
    }
}

impl From<crate::arch32::sigset64_t> for crate::sigset_t {
    fn from(sigset: crate::arch32::sigset64_t) -> Self {
        Self { sig: [sigset.sig[0].into()] }
    }
}

impl From<crate::sigset_t> for crate::arch32::sigset64_t {
    fn from(sigset: crate::sigset_t) -> Self {
        Self { sig: [sigset.sig[0] as u32] }
    }
}

arch_translate_data! {
    BidiFrom<__kernel_sigaction> {
        sa_handler,
        sa_mask,
        sa_flags,
        sa_restorer,
    }
}

translate_data! {
    BidiTryFrom<crate::arch32::sigaction64, crate::__kernel_sigaction> {
        sa_handler = sa_handler;
        sa_mask = sa_mask;
        sa_flags = sa_flags;
        sa_restorer = sa_restorer;
    }
}
impl From<crate::arch32::sigval> for crate::sigval {
    fn from(sigval: crate::arch32::sigval) -> Self {
        // SAFETY: This is safe because the union has a single field.
        let bindgen_opaque_blob = unsafe { sigval._bindgen_opaque_blob };
        Self { _bindgen_opaque_blob: bindgen_opaque_blob.into() }
    }
}

impl TryFrom<crate::sigval> for crate::arch32::sigval {
    type Error = ();
    fn try_from(sigval: crate::sigval) -> Result<Self, ()> {
        // SAFETY: This is safe because the union has a single field.
        let bindgen_opaque_blob = unsafe { sigval._bindgen_opaque_blob };
        Ok(Self { _bindgen_opaque_blob: bindgen_opaque_blob.try_into().map_err(|_| ())? })
    }
}

impl From<crate::arch32::sigevent> for crate::sigevent {
    fn from(sigevent: crate::arch32::sigevent) -> Self {
        let mut _sigev_un = crate::sigevent__bindgen_ty_1::default();
        // SAFETY: This is safe because the union has all bytes defined.
        match sigevent.sigev_notify as u32 {
            crate::SIGEV_THREAD_ID => unsafe {
                _sigev_un._tid = sigevent._sigev_un._tid.into();
            },
            crate::SIGEV_THREAD => unsafe {
                _sigev_un._sigev_thread = crate::sigevent__bindgen_ty_1__bindgen_ty_1 {
                    _function: sigevent._sigev_un._sigev_thread._function.into(),
                    _attribute: sigevent._sigev_un._sigev_thread._attribute.into(),
                };
            },
            _ => {}
        }
        Self {
            sigev_value: sigevent.sigev_value.into(),
            sigev_signo: sigevent.sigev_signo.into(),
            sigev_notify: sigevent.sigev_notify.into(),
            _sigev_un,
        }
    }
}

impl TryFrom<crate::sigevent> for crate::arch32::sigevent {
    type Error = ();
    fn try_from(sigevent: crate::sigevent) -> Result<Self, ()> {
        let mut _sigev_un = crate::arch32::sigevent__bindgen_ty_1::default();
        // SAFETY: This is safe because the union has all bytes defined.
        match sigevent.sigev_notify as u32 {
            crate::SIGEV_THREAD_ID => unsafe {
                _sigev_un._tid = sigevent._sigev_un._tid.try_into().map_err(|_| ())?;
            },
            crate::SIGEV_THREAD => unsafe {
                _sigev_un._sigev_thread = crate::arch32::sigevent__bindgen_ty_1__bindgen_ty_1 {
                    _function: sigevent
                        ._sigev_un
                        ._sigev_thread
                        ._function
                        .try_into()
                        .map_err(|_| ())?,
                    _attribute: sigevent
                        ._sigev_un
                        ._sigev_thread
                        ._attribute
                        .try_into()
                        .map_err(|_| ())?,
                };
            },
            _ => {}
        }
        Ok(Self {
            sigev_value: sigevent.sigev_value.try_into().map_err(|_| ())?,
            sigev_signo: sigevent.sigev_signo.try_into().map_err(|_| ())?,
            sigev_notify: sigevent.sigev_notify.try_into().map_err(|_| ())?,
            _sigev_un,
        })
    }
}
