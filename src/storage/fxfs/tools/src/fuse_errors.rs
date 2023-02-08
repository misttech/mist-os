// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuse3::{Errno, Result},
    fxfs::{errors::FxfsError, log::info},
    libc,
};

/// Convert from Fxfs error to libc error.
pub fn cast_to_fuse_error(err: &anyhow::Error) -> Errno {
    if let Some(root_cause) = err.root_cause().downcast_ref::<FxfsError>() {
        let err = match root_cause {
            FxfsError::NotFound => libc::ENOENT.into(),
            FxfsError::NotDir => libc::ENOTDIR.into(),
            FxfsError::Deleted => libc::ENOENT.into(),
            FxfsError::AlreadyExists => libc::EEXIST.into(),
            FxfsError::NotFile => libc::EISDIR.into(),
            FxfsError::NotEmpty => libc::ENOTEMPTY.into(),
            FxfsError::ReadOnlyFilesystem => libc::EROFS.into(),
            FxfsError::NoSpace => libc::ENOSPC.into(),
            FxfsError::InvalidArgs => libc::EINVAL.into(),
            FxfsError::TooBig => libc::EFBIG.into(),
            FxfsError::InvalidVersion => libc::ENOTSUP.into(),
            FxfsError::JournalFlushError => libc::EIO.into(),
            FxfsError::NotSupported => libc::ENOTSUP.into(),
            FxfsError::AccessDenied => libc::EACCES.into(),
            FxfsError::OutOfRange => libc::ERANGE.into(),
            FxfsError::BadPath => libc::ENAMETOOLONG.into(),
            FxfsError::AlreadyBound => libc::EBUSY.into(),
            FxfsError::Inconsistent => libc::ENOTTY.into(),
            FxfsError::Internal => libc::ENOTSUP.into(),
        };
        info!("Converted from Fxfs error {:?} to libc error {:?}", root_cause, err);
        err
    } else {
        libc::ENOTSUP.into()
    }
}

pub trait FuseErrorParser<S> {
    /// Convert from Fxfs Result to Fuse Result.
    fn parse_error(self) -> Result<S>;
}

impl<S> FuseErrorParser<S> for std::result::Result<S, anyhow::Error> {
    fn parse_error(self) -> Result<S> {
        if let Ok(ret) = self {
            Ok(ret)
        } else {
            let err = &self.err().unwrap();
            Err(cast_to_fuse_error(err))
        }
    }
}
