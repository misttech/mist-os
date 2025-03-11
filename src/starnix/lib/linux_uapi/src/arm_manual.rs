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

macro_rules! translate_data {
    ($type_name:ident; $( $field :ident ),+) => {
      translate_data!(FROM32; $type_name; $( $field ),*);
      translate_data!(TRYFROM64; $type_name; $( $field ),*);
    };
    (FROM32; $type_name:ident; $( $field :ident ),+) => {
        impl From<crate::arch32::$type_name> for crate::$type_name {
            fn from($type_name: crate::arch32::$type_name) -> Self {
                Self {
                    $( $field: $type_name.$field.into(),  )*
                    ..Default::default()
                }
            }
        }

    };
    (TRYFROM64; $type_name:ident; $( $field :ident ),+) => {
        impl TryFrom<crate::$type_name> for crate::arch32::$type_name {
            type Error = ();
            fn try_from($type_name: crate::$type_name) -> Result<Self, ()> {
                Ok(Self {
                    $( $field: $type_name.$field.try_into().map_err(|_| ())?,  )*
                    ..Default::default()
                })
            }
        }
    };
}

translate_data!(flock; l_type, l_whence, l_start, l_len, l_pid);
translate_data!(itimerspec; it_interval, it_value);
translate_data!(itimerval; it_interval, it_value);
translate_data!(__kernel_fsid_t; val);
translate_data!(sigaltstack; ss_sp, ss_flags, ss_size);

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

translate_data!(FROM32; timespec; tv_sec, tv_nsec);
impl From<crate::timespec> for crate::arch32::timespec {
    fn from(tv: crate::timespec) -> Self {
        Self {
            tv_sec: saturating_i64_to_i32(tv.tv_sec),
            tv_nsec: saturating_i64_to_i32(tv.tv_nsec),
        }
    }
}

translate_data!(FROM32; timeval; tv_sec, tv_usec);
impl From<crate::timeval> for crate::arch32::timeval {
    fn from(tv: crate::timeval) -> Self {
        Self {
            tv_sec: saturating_i64_to_i32(tv.tv_sec),
            tv_usec: saturating_i64_to_i32(tv.tv_usec),
        }
    }
}

translate_data!(FROM32; rlimit; rlim_cur, rlim_max);
impl From<crate::rlimit> for crate::arch32::rlimit {
    fn from(rlimit: crate::rlimit) -> Self {
        Self {
            rlim_cur: saturating_u64_to_u32(rlimit.rlim_cur),
            rlim_max: saturating_u64_to_u32(rlimit.rlim_max),
        }
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
            f_fsid: statfs.f_fsid.try_into().map_err(|_| ())?,
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

impl From<crate::arch32::sigaction64> for crate::__kernel_sigaction {
    fn from(sigaction: crate::arch32::sigaction64) -> Self {
        Self {
            sa_handler: sigaction.sa_handler.into(),
            sa_mask: sigaction.sa_mask.into(),
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

impl TryFrom<crate::__kernel_sigaction> for crate::arch32::sigaction64 {
    type Error = ();
    fn try_from(sigaction: crate::__kernel_sigaction) -> Result<Self, ()> {
        Ok(Self {
            sa_handler: sigaction.sa_handler.try_into().map_err(|_| ())?,
            sa_mask: sigaction.sa_mask.try_into().map_err(|_| ())?,
            sa_flags: sigaction.sa_flags.try_into().map_err(|_| ())?,
            sa_restorer: sigaction.sa_restorer.try_into().map_err(|_| ())?,
        })
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
