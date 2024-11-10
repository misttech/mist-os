// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use crate::uapi;

use std::fmt::{Debug, Display, Formatter};

pub struct Errno {
    pub code: ErrnoCode,
    anyhow: Option<anyhow::Error>,
}

impl Errno {
    #[track_caller]
    pub fn new(code: ErrnoCode, name: &'static str, context: Option<String>) -> Errno {
        Errno {
            code,
            anyhow: Some(anyhow::format_err!(
                "{} ({}), source: {}, context: {}",
                name,
                code,
                std::panic::Location::caller(),
                context.as_ref().unwrap_or(&"None".to_string())
            )),
        }
    }

    /// Returns a new `Result` that is an `Err` with an `Errno` with the given `code`. This error
    /// has no context, and as such, is cheap to built.
    pub fn fail<A>(code: ErrnoCode) -> Result<A, Errno> {
        Err(Errno { code, anyhow: None })
    }

    pub fn return_value(&self) -> u64 {
        self.code.return_value()
    }
}

impl Clone for Errno {
    fn clone(&self) -> Self {
        let anyhow = self.anyhow.as_ref().map(|e| anyhow::anyhow!(e.to_string()));
        Errno { code: self.code, anyhow }
    }
}

impl PartialEq for Errno {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
    }
}

impl PartialEq<ErrnoCode> for Errno {
    fn eq(&self, other: &ErrnoCode) -> bool {
        self.code == *other
    }
}

impl Eq for Errno {}

impl Display for Errno {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.anyhow {
            Some(details) => {
                write!(f, "errno {}, details: {details}", self.code)
            }
            None => {
                write!(f, "errno {} from unknown location", self.code)
            }
        }
    }
}

impl std::error::Error for Errno {}

impl From<Errno> for zx_status::Status {
    fn from(e: Errno) -> Self {
        match e.code.error_code() {
            uapi::ENOENT => zx_status::Status::NOT_FOUND,
            uapi::ENOMEM => zx_status::Status::NO_MEMORY,
            uapi::EINVAL => zx_status::Status::INVALID_ARGS,
            uapi::ETIMEDOUT => zx_status::Status::TIMED_OUT,
            uapi::EBUSY => zx_status::Status::UNAVAILABLE,
            uapi::EEXIST => zx_status::Status::ALREADY_EXISTS,
            uapi::EPIPE => zx_status::Status::PEER_CLOSED,
            uapi::ENAMETOOLONG => zx_status::Status::BAD_PATH,
            uapi::EIO => zx_status::Status::IO,
            uapi::EISDIR => zx_status::Status::NOT_FILE,
            uapi::ENOTDIR => zx_status::Status::NOT_DIR,
            uapi::EOPNOTSUPP => zx_status::Status::NOT_SUPPORTED,
            uapi::EBADF => zx_status::Status::BAD_HANDLE,
            uapi::EACCES => zx_status::Status::ACCESS_DENIED,
            uapi::EAGAIN => zx_status::Status::SHOULD_WAIT,
            uapi::EFBIG => zx_status::Status::FILE_BIG,
            uapi::ENOSPC => zx_status::Status::NO_SPACE,
            uapi::ENOTEMPTY => zx_status::Status::NOT_EMPTY,
            uapi::EPROTONOSUPPORT => zx_status::Status::PROTOCOL_NOT_SUPPORTED,
            uapi::ENETUNREACH => zx_status::Status::ADDRESS_UNREACHABLE,
            uapi::EADDRINUSE => zx_status::Status::ADDRESS_IN_USE,
            uapi::ENOTCONN => zx_status::Status::NOT_CONNECTED,
            uapi::ECONNREFUSED => zx_status::Status::CONNECTION_REFUSED,
            uapi::ECONNRESET => zx_status::Status::CONNECTION_RESET,
            uapi::ECONNABORTED => zx_status::Status::CONNECTION_ABORTED,
            _ => zx_status::Status::NOT_SUPPORTED,
        }
    }
}

impl Debug for Errno {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(err) = self.anyhow.as_ref() {
            Debug::fmt(&err, f)
        } else {
            write!(f, "error {} from unknown location", self.code)
        }
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct ErrnoCode(u32);

impl ErrnoCode {
    pub fn from_return_value(retval: u64) -> Self {
        let retval = retval as i64;
        if retval >= 0 {
            // Collapse all success codes to 0. This is the only value in the u32 range which
            // is guaranteed to not be an error code.
            return Self(0);
        }
        Self(-retval as u32)
    }

    pub fn from_error_code(code: i16) -> Self {
        Self(code as u32)
    }

    pub fn return_value(&self) -> u64 {
        -(self.0 as i32) as u64
    }

    pub fn error_code(&self) -> u32 {
        self.0
    }
}

impl Display for ErrnoCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Special errors indicating a blocking syscall was interrupted, but it can be restarted.
//
// They are not defined in uapi, but can be observed by ptrace on Linux.
//
// If the syscall is restartable, it might not be restarted, depending on the value of SA_RESTART
// for the signal handler and the specific restartable error code.
// But it will always be restarted if the signal did not call a userspace signal handler.
// If not restarted, this error code is converted into EINTR.
//
// More information can be found at
// https://cs.opensource.google/gvisor/gvisor/+/master:pkg/errors/linuxerr/internal.go;l=71;drc=2bb73c7bd7dcf0b36e774d8e82e464d04bc81f4b.

/// Convert to EINTR if interrupted by a signal handler without SA_RESTART enabled, otherwise
/// restart.
pub const ERESTARTSYS: ErrnoCode = ErrnoCode(512);

/// Always restart, regardless of the signal handler.
pub const ERESTARTNOINTR: ErrnoCode = ErrnoCode(513);

/// Convert to EINTR if interrupted by a signal handler. SA_RESTART is ignored. Otherwise restart.
pub const ERESTARTNOHAND: ErrnoCode = ErrnoCode(514);

/// Like `ERESTARTNOHAND`, but restart by invoking a closure instead of calling the syscall
/// implementation again.
pub const ERESTART_RESTARTBLOCK: ErrnoCode = ErrnoCode(516);

/// An extension trait for `Result<T, Errno>`.
pub trait ErrnoResultExt<T> {
    /// Maps `Err(EINTR)` to the specified errno.
    fn map_eintr(self, make_errno: impl Fn() -> Errno) -> Result<T, Errno>;
}

impl<T> ErrnoResultExt<T> for Result<T, Errno> {
    fn map_eintr(self, make_errno: impl Fn() -> Errno) -> Result<T, Errno> {
        self.map_err(|err| if err == EINTR { make_errno() } else { err })
    }
}

pub const EPERM: ErrnoCode = ErrnoCode(uapi::EPERM);
pub const ENOENT: ErrnoCode = ErrnoCode(uapi::ENOENT);
pub const ESRCH: ErrnoCode = ErrnoCode(uapi::ESRCH);
pub const EINTR: ErrnoCode = ErrnoCode(uapi::EINTR);
pub const EIO: ErrnoCode = ErrnoCode(uapi::EIO);
pub const ENXIO: ErrnoCode = ErrnoCode(uapi::ENXIO);
pub const E2BIG: ErrnoCode = ErrnoCode(uapi::E2BIG);
pub const ENOEXEC: ErrnoCode = ErrnoCode(uapi::ENOEXEC);
pub const EBADF: ErrnoCode = ErrnoCode(uapi::EBADF);
pub const ECHILD: ErrnoCode = ErrnoCode(uapi::ECHILD);
pub const EAGAIN: ErrnoCode = ErrnoCode(uapi::EAGAIN);
pub const ENOMEM: ErrnoCode = ErrnoCode(uapi::ENOMEM);
pub const EACCES: ErrnoCode = ErrnoCode(uapi::EACCES);
pub const EFAULT: ErrnoCode = ErrnoCode(uapi::EFAULT);
pub const ENOTBLK: ErrnoCode = ErrnoCode(uapi::ENOTBLK);
pub const EBUSY: ErrnoCode = ErrnoCode(uapi::EBUSY);
pub const EEXIST: ErrnoCode = ErrnoCode(uapi::EEXIST);
pub const EXDEV: ErrnoCode = ErrnoCode(uapi::EXDEV);
pub const ENODEV: ErrnoCode = ErrnoCode(uapi::ENODEV);
pub const ENOTDIR: ErrnoCode = ErrnoCode(uapi::ENOTDIR);
pub const EISDIR: ErrnoCode = ErrnoCode(uapi::EISDIR);
pub const EINVAL: ErrnoCode = ErrnoCode(uapi::EINVAL);
pub const ENFILE: ErrnoCode = ErrnoCode(uapi::ENFILE);
pub const EMFILE: ErrnoCode = ErrnoCode(uapi::EMFILE);
pub const ENOTTY: ErrnoCode = ErrnoCode(uapi::ENOTTY);
pub const ETXTBSY: ErrnoCode = ErrnoCode(uapi::ETXTBSY);
pub const EFBIG: ErrnoCode = ErrnoCode(uapi::EFBIG);
pub const ENOSPC: ErrnoCode = ErrnoCode(uapi::ENOSPC);
pub const ESPIPE: ErrnoCode = ErrnoCode(uapi::ESPIPE);
pub const EROFS: ErrnoCode = ErrnoCode(uapi::EROFS);
pub const EMLINK: ErrnoCode = ErrnoCode(uapi::EMLINK);
pub const EPIPE: ErrnoCode = ErrnoCode(uapi::EPIPE);
pub const EDOM: ErrnoCode = ErrnoCode(uapi::EDOM);
pub const ERANGE: ErrnoCode = ErrnoCode(uapi::ERANGE);
pub const EDEADLK: ErrnoCode = ErrnoCode(uapi::EDEADLK);
pub const ENAMETOOLONG: ErrnoCode = ErrnoCode(uapi::ENAMETOOLONG);
pub const ENOLCK: ErrnoCode = ErrnoCode(uapi::ENOLCK);
pub const ENOSYS: ErrnoCode = ErrnoCode(uapi::ENOSYS);
pub const ENOTEMPTY: ErrnoCode = ErrnoCode(uapi::ENOTEMPTY);
pub const ELOOP: ErrnoCode = ErrnoCode(uapi::ELOOP);
pub const EWOULDBLOCK: ErrnoCode = ErrnoCode(uapi::EWOULDBLOCK);
pub const ENOMSG: ErrnoCode = ErrnoCode(uapi::ENOMSG);
pub const EIDRM: ErrnoCode = ErrnoCode(uapi::EIDRM);
pub const ECHRNG: ErrnoCode = ErrnoCode(uapi::ECHRNG);
pub const EL2NSYNC: ErrnoCode = ErrnoCode(uapi::EL2NSYNC);
pub const EL3HLT: ErrnoCode = ErrnoCode(uapi::EL3HLT);
pub const EL3RST: ErrnoCode = ErrnoCode(uapi::EL3RST);
pub const ELNRNG: ErrnoCode = ErrnoCode(uapi::ELNRNG);
pub const EUNATCH: ErrnoCode = ErrnoCode(uapi::EUNATCH);
pub const ENOCSI: ErrnoCode = ErrnoCode(uapi::ENOCSI);
pub const EL2HLT: ErrnoCode = ErrnoCode(uapi::EL2HLT);
pub const EBADE: ErrnoCode = ErrnoCode(uapi::EBADE);
pub const EBADR: ErrnoCode = ErrnoCode(uapi::EBADR);
pub const EXFULL: ErrnoCode = ErrnoCode(uapi::EXFULL);
pub const ENOANO: ErrnoCode = ErrnoCode(uapi::ENOANO);
pub const EBADRQC: ErrnoCode = ErrnoCode(uapi::EBADRQC);
pub const EBADSLT: ErrnoCode = ErrnoCode(uapi::EBADSLT);
pub const EDEADLOCK: ErrnoCode = ErrnoCode(uapi::EDEADLOCK);
pub const EBFONT: ErrnoCode = ErrnoCode(uapi::EBFONT);
pub const ENOSTR: ErrnoCode = ErrnoCode(uapi::ENOSTR);
pub const ENODATA: ErrnoCode = ErrnoCode(uapi::ENODATA);
pub const ETIME: ErrnoCode = ErrnoCode(uapi::ETIME);
pub const ENOSR: ErrnoCode = ErrnoCode(uapi::ENOSR);
pub const ENONET: ErrnoCode = ErrnoCode(uapi::ENONET);
pub const ENOPKG: ErrnoCode = ErrnoCode(uapi::ENOPKG);
pub const EREMOTE: ErrnoCode = ErrnoCode(uapi::EREMOTE);
pub const ENOLINK: ErrnoCode = ErrnoCode(uapi::ENOLINK);
pub const EADV: ErrnoCode = ErrnoCode(uapi::EADV);
pub const ESRMNT: ErrnoCode = ErrnoCode(uapi::ESRMNT);
pub const ECOMM: ErrnoCode = ErrnoCode(uapi::ECOMM);
pub const EPROTO: ErrnoCode = ErrnoCode(uapi::EPROTO);
pub const EMULTIHOP: ErrnoCode = ErrnoCode(uapi::EMULTIHOP);
pub const EDOTDOT: ErrnoCode = ErrnoCode(uapi::EDOTDOT);
pub const EBADMSG: ErrnoCode = ErrnoCode(uapi::EBADMSG);
pub const EOVERFLOW: ErrnoCode = ErrnoCode(uapi::EOVERFLOW);
pub const ENOTUNIQ: ErrnoCode = ErrnoCode(uapi::ENOTUNIQ);
pub const EBADFD: ErrnoCode = ErrnoCode(uapi::EBADFD);
pub const EREMCHG: ErrnoCode = ErrnoCode(uapi::EREMCHG);
pub const ELIBACC: ErrnoCode = ErrnoCode(uapi::ELIBACC);
pub const ELIBBAD: ErrnoCode = ErrnoCode(uapi::ELIBBAD);
pub const ELIBSCN: ErrnoCode = ErrnoCode(uapi::ELIBSCN);
pub const ELIBMAX: ErrnoCode = ErrnoCode(uapi::ELIBMAX);
pub const ELIBEXEC: ErrnoCode = ErrnoCode(uapi::ELIBEXEC);
pub const EILSEQ: ErrnoCode = ErrnoCode(uapi::EILSEQ);
pub const ERESTART: ErrnoCode = ErrnoCode(uapi::ERESTART);
pub const ESTRPIPE: ErrnoCode = ErrnoCode(uapi::ESTRPIPE);
pub const EUSERS: ErrnoCode = ErrnoCode(uapi::EUSERS);
pub const ENOTSOCK: ErrnoCode = ErrnoCode(uapi::ENOTSOCK);
pub const EDESTADDRREQ: ErrnoCode = ErrnoCode(uapi::EDESTADDRREQ);
pub const EMSGSIZE: ErrnoCode = ErrnoCode(uapi::EMSGSIZE);
pub const EPROTOTYPE: ErrnoCode = ErrnoCode(uapi::EPROTOTYPE);
pub const ENOPROTOOPT: ErrnoCode = ErrnoCode(uapi::ENOPROTOOPT);
pub const EPROTONOSUPPORT: ErrnoCode = ErrnoCode(uapi::EPROTONOSUPPORT);
pub const ESOCKTNOSUPPORT: ErrnoCode = ErrnoCode(uapi::ESOCKTNOSUPPORT);
pub const EOPNOTSUPP: ErrnoCode = ErrnoCode(uapi::EOPNOTSUPP);
pub const EPFNOSUPPORT: ErrnoCode = ErrnoCode(uapi::EPFNOSUPPORT);
pub const EAFNOSUPPORT: ErrnoCode = ErrnoCode(uapi::EAFNOSUPPORT);
pub const EADDRINUSE: ErrnoCode = ErrnoCode(uapi::EADDRINUSE);
pub const EADDRNOTAVAIL: ErrnoCode = ErrnoCode(uapi::EADDRNOTAVAIL);
pub const ENETDOWN: ErrnoCode = ErrnoCode(uapi::ENETDOWN);
pub const ENETUNREACH: ErrnoCode = ErrnoCode(uapi::ENETUNREACH);
pub const ENETRESET: ErrnoCode = ErrnoCode(uapi::ENETRESET);
pub const ECONNABORTED: ErrnoCode = ErrnoCode(uapi::ECONNABORTED);
pub const ECONNRESET: ErrnoCode = ErrnoCode(uapi::ECONNRESET);
pub const ENOBUFS: ErrnoCode = ErrnoCode(uapi::ENOBUFS);
pub const EISCONN: ErrnoCode = ErrnoCode(uapi::EISCONN);
pub const ENOTCONN: ErrnoCode = ErrnoCode(uapi::ENOTCONN);
pub const ESHUTDOWN: ErrnoCode = ErrnoCode(uapi::ESHUTDOWN);
pub const ETOOMANYREFS: ErrnoCode = ErrnoCode(uapi::ETOOMANYREFS);
pub const ETIMEDOUT: ErrnoCode = ErrnoCode(uapi::ETIMEDOUT);
pub const ECONNREFUSED: ErrnoCode = ErrnoCode(uapi::ECONNREFUSED);
pub const EHOSTDOWN: ErrnoCode = ErrnoCode(uapi::EHOSTDOWN);
pub const EHOSTUNREACH: ErrnoCode = ErrnoCode(uapi::EHOSTUNREACH);
pub const EALREADY: ErrnoCode = ErrnoCode(uapi::EALREADY);
pub const EINPROGRESS: ErrnoCode = ErrnoCode(uapi::EINPROGRESS);
pub const ESTALE: ErrnoCode = ErrnoCode(uapi::ESTALE);
pub const EUCLEAN: ErrnoCode = ErrnoCode(uapi::EUCLEAN);
pub const ENOTNAM: ErrnoCode = ErrnoCode(uapi::ENOTNAM);
pub const ENAVAIL: ErrnoCode = ErrnoCode(uapi::ENAVAIL);
pub const EISNAM: ErrnoCode = ErrnoCode(uapi::EISNAM);
pub const EREMOTEIO: ErrnoCode = ErrnoCode(uapi::EREMOTEIO);
pub const EDQUOT: ErrnoCode = ErrnoCode(uapi::EDQUOT);
pub const ENOMEDIUM: ErrnoCode = ErrnoCode(uapi::ENOMEDIUM);
pub const EMEDIUMTYPE: ErrnoCode = ErrnoCode(uapi::EMEDIUMTYPE);
pub const ECANCELED: ErrnoCode = ErrnoCode(uapi::ECANCELED);
pub const ENOKEY: ErrnoCode = ErrnoCode(uapi::ENOKEY);
pub const EKEYEXPIRED: ErrnoCode = ErrnoCode(uapi::EKEYEXPIRED);
pub const EKEYREVOKED: ErrnoCode = ErrnoCode(uapi::EKEYREVOKED);
pub const EKEYREJECTED: ErrnoCode = ErrnoCode(uapi::EKEYREJECTED);
pub const EOWNERDEAD: ErrnoCode = ErrnoCode(uapi::EOWNERDEAD);
pub const ENOTRECOVERABLE: ErrnoCode = ErrnoCode(uapi::ENOTRECOVERABLE);
pub const ERFKILL: ErrnoCode = ErrnoCode(uapi::ERFKILL);
pub const EHWPOISON: ErrnoCode = ErrnoCode(uapi::EHWPOISON);

// ENOTSUP is a different error in posix, but has the same value as EOPNOTSUPP in linux.
pub const ENOTSUP: ErrnoCode = EOPNOTSUPP;

/// `errno` returns an `Errno` struct tagged with the current file name and line number.
///
/// Use `error!` instead if you want the `Errno` to be wrapped in an `Err`.
#[macro_export]
macro_rules! errno {
    ($err:ident) => {
        $crate::errors::Errno::new($crate::errors::$err, stringify!($err), None)
    };
    ($err:ident, $context:expr) => {
        $crate::errors::Errno::new(
            $crate::errors::$err,
            stringify!($err),
            Some($context.to_string()),
        )
    };
}

/// `error` returns a `Err` containing an `Errno` struct tagged with the current file name and line
/// number.
///
/// Use `errno!` instead if you want an unwrapped, but still tagged, `Errno`.
#[macro_export]
macro_rules! error {
    ($($args:tt)*) => { Err($crate::errno!($($args)*)) };
}

/// `errno_from_code` returns a `Err` containing an `Errno` struct with the given error code and is
/// tagged with the current file name and line number.
#[macro_export]
macro_rules! errno_from_code {
    ($err:expr) => {{
        let errno = $crate::errors::ErrnoCode::from_error_code($err);
        $crate::errors::Errno::new(errno, stringify!($err), None)
    }};
}

/// `errno_from_zxio_code` returns an `Errno` struct with the given error code and is
/// tagged with the current file name and line number.
#[macro_export]
macro_rules! errno_from_zxio_code {
    ($err:expr) => {{
        let code = $err.raw();
        $crate::errno_from_code!(code)
    }};
}

// There isn't really a mapping from zx_status::Status to Errno. The correct mapping is
// context-specific but this converter is a reasonable first-approximation. The translation matches
// fdio_status_to_errno. See https://fxbug.dev/42105838 for more context.
// TODO: Replace clients with more context-specific mappings.
#[macro_export]
macro_rules! from_status_like_fdio {
    ($status:expr) => {{
        $crate::from_status_like_fdio!($status, "")
    }};
    ($status:expr, $context:expr) => {{
        match $status {
            zx_status::Status::NOT_FOUND => $crate::errno!(ENOENT, $context),
            zx_status::Status::NO_MEMORY => $crate::errno!(ENOMEM, $context),
            zx_status::Status::INVALID_ARGS => $crate::errno!(EINVAL, $context),
            zx_status::Status::BUFFER_TOO_SMALL => $crate::errno!(EINVAL, $context),
            zx_status::Status::TIMED_OUT => $crate::errno!(ETIMEDOUT, $context),
            zx_status::Status::UNAVAILABLE => $crate::errno!(EBUSY, $context),
            zx_status::Status::ALREADY_EXISTS => $crate::errno!(EEXIST, $context),
            zx_status::Status::PEER_CLOSED => $crate::errno!(EPIPE, $context),
            zx_status::Status::BAD_STATE => $crate::errno!(EPIPE, $context),
            zx_status::Status::BAD_PATH => $crate::errno!(ENAMETOOLONG, $context),
            zx_status::Status::IO => $crate::errno!(EIO, $context),
            zx_status::Status::NOT_FILE => $crate::errno!(EISDIR, $context),
            zx_status::Status::NOT_DIR => $crate::errno!(ENOTDIR, $context),
            zx_status::Status::NOT_SUPPORTED => $crate::errno!(EOPNOTSUPP, $context),
            zx_status::Status::WRONG_TYPE => $crate::errno!(EOPNOTSUPP, $context),
            zx_status::Status::OUT_OF_RANGE => $crate::errno!(EINVAL, $context),
            zx_status::Status::NO_RESOURCES => $crate::errno!(ENOMEM, $context),
            zx_status::Status::BAD_HANDLE => $crate::errno!(EBADF, $context),
            zx_status::Status::ACCESS_DENIED => $crate::errno!(EACCES, $context),
            zx_status::Status::SHOULD_WAIT => $crate::errno!(EAGAIN, $context),
            zx_status::Status::FILE_BIG => $crate::errno!(EFBIG, $context),
            zx_status::Status::NO_SPACE => $crate::errno!(ENOSPC, $context),
            zx_status::Status::NOT_EMPTY => $crate::errno!(ENOTEMPTY, $context),
            zx_status::Status::IO_REFUSED => $crate::errno!(ECONNREFUSED, $context),
            zx_status::Status::IO_INVALID => $crate::errno!(EIO, $context),
            zx_status::Status::CANCELED => $crate::errno!(EBADF, $context),
            zx_status::Status::PROTOCOL_NOT_SUPPORTED => {
                $crate::errno!(EPROTONOSUPPORT, $context)
            }
            zx_status::Status::ADDRESS_UNREACHABLE => $crate::errno!(ENETUNREACH, $context),
            zx_status::Status::ADDRESS_IN_USE => $crate::errno!(EADDRINUSE, $context),
            zx_status::Status::NOT_CONNECTED => $crate::errno!(ENOTCONN, $context),
            zx_status::Status::CONNECTION_REFUSED => $crate::errno!(ECONNREFUSED, $context),
            zx_status::Status::CONNECTION_RESET => $crate::errno!(ECONNRESET, $context),
            zx_status::Status::CONNECTION_ABORTED => $crate::errno!(ECONNABORTED, $context),
            _ => $crate::errno!(EIO, $context),
        }
    }};
}

// Public re-export of macros allows them to be used like regular rust items.
pub use {errno, errno_from_code, errno_from_zxio_code, error, from_status_like_fdio};

pub trait SourceContext<T, E> {
    /// Similar to `with_context` in [`anyhow::Context`], but adds the source location of the
    /// caller. This is especially useful when generating a message to attach to an error object as
    /// the context.
    ///
    /// Example:
    ///
    ///     let mount_point = system_task
    ///         .lookup_path_from_root(locked, mount_point)
    ///         .with_source_context(|| {
    ///             format!("lookup path from root: {}", String::from_utf8_lossy(mount_point))
    ///         })?;
    ///
    /// Results in a context message like:
    ///
    ///     Caused by:
    ///         1: lookup path from root: ..., some_file.rs:12:34
    ///
    #[track_caller]
    fn with_source_context(self, context: impl FnOnce() -> String) -> anyhow::Result<T>;

    /// Similar to `context` in [`anyhow::Context`], but adds the source location of the caller.
    /// This is especially useful when generating a message to attach to an error object as the
    /// context.
    #[track_caller]
    fn source_context<C>(self, context: C) -> anyhow::Result<T>
    where
        C: Display + Send + Sync + 'static;
}

/// All result objects that support `with_context` will also support `with_source_context`.
impl<T, E> SourceContext<T, E> for Result<T, E>
where
    Result<T, E>: anyhow::Context<T, E>,
{
    fn with_source_context(self, context: impl FnOnce() -> String) -> anyhow::Result<T> {
        use anyhow::Context;
        let caller = std::panic::Location::caller();
        self.with_context(move || format!("{}, {}", context(), caller))
    }

    fn source_context<C>(self, context: C) -> anyhow::Result<T>
    where
        C: Display + Send + Sync + 'static,
    {
        use anyhow::Context;
        let caller = std::panic::Location::caller();
        self.context(format!("{}, {}", context, caller))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::Location;

    #[test]
    fn with_source_context() {
        let errno = Errno { code: ENOENT, anyhow: None };
        let result: Result<(), Errno> = Err(errno);
        let error = result.with_source_context(|| format!("42")).unwrap_err();
        let line_after_error = Location::caller();
        let expected_prefix =
            format!("42, {}:{}:", line_after_error.file(), line_after_error.line() - 1,);
        assert!(
            error.to_string().starts_with(&expected_prefix),
            "{:?} must start with {:?}",
            error,
            expected_prefix
        );
    }

    #[test]
    fn source_context() {
        let errno = Errno { code: ENOENT, anyhow: None };
        let result: Result<(), Errno> = Err(errno);
        let error = result.source_context("42").unwrap_err();
        let line_after_error = Location::caller();
        let expected_prefix =
            format!("42, {}:{}:", line_after_error.file(), line_after_error.line() - 1,);
        assert!(
            error.to_string().starts_with(&expected_prefix),
            "{:?} must start with {:?}",
            error,
            expected_prefix
        );
    }
}
