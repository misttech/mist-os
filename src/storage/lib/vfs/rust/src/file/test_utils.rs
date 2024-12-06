// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common utilities used by pseudo-file related tests.

use crate::directory::entry::DirectoryEntry;
use crate::test_utils::run::{self, AsyncServerClientTestParams};

use fidl_fuchsia_io as fio;
use futures::Future;
use std::sync::Arc;
use zx_status::Status;

pub use run::{run_client, test_client};

/// A thin wrapper around [`run::run_server_client()`] that sets the `Marker` to be
/// [`FileMarker`], and providing explicit type for the `get_client` closure argument.  This makes
/// it possible for the caller not to provide explicit types.
pub fn run_server_client<GetClientRes>(
    flags: fio::OpenFlags,
    server: Arc<dyn DirectoryEntry>,
    get_client: impl FnOnce(fio::FileProxy) -> GetClientRes,
) where
    GetClientRes: Future<Output = ()>,
{
    run::run_server_client::<fio::FileMarker, _, _>(flags, server, get_client)
}

/// A thin wrapper around [`run::test_server_client()`] that sets the `Marker` to be
/// [`FileMarker`], and providing explicit type for the `get_client` closure argument.  This makes
/// it possible for the caller not to provide explicit types.
pub fn test_server_client<'test_refs, GetClientRes>(
    flags: fio::OpenFlags,
    server: Arc<dyn DirectoryEntry>,
    get_client: impl FnOnce(fio::FileProxy) -> GetClientRes + 'test_refs,
) -> AsyncServerClientTestParams<'test_refs, fio::FileMarker>
where
    GetClientRes: Future<Output = ()> + 'test_refs,
{
    run::test_server_client::<fio::FileMarker, _, _>(flags, server, get_client)
}

/// Possible errors for the [`assert_vmo_content()`] function.
pub enum AssertVmoContentError {
    /// Failure returned from the `vmo.get_content_size()` call.
    GetContentSizeFailed(Status),
    /// Failure returned from the `vmo.read()` call.
    VmoReadFailed(Status),
    /// Expected content and the actual VMO content did not match.
    UnexpectedContent(Vec<u8>),
}

/// Reads the VMO content and matches it against the expectation.
#[cfg(target_os = "fuchsia")]
pub fn assert_vmo_content(vmo: &fidl::Vmo, expected: &[u8]) -> Result<(), AssertVmoContentError> {
    let size =
        vmo.get_content_size().map_err(AssertVmoContentError::GetContentSizeFailed)? as usize;
    let mut buffer = vec![0; size];
    vmo.read(&mut buffer, 0).map_err(AssertVmoContentError::VmoReadFailed)?;
    if buffer != expected {
        Err(AssertVmoContentError::UnexpectedContent(buffer))
    } else {
        Ok(())
    }
}

/// Wraps an [`assert_vmo_content()`] call, panicking with a descriptive error message for any `Err`
/// return values.
#[macro_export]
macro_rules! assert_vmo_content {
    ($vmo:expr, $expected:expr) => {{
        use $crate::file::test_utils::{assert_vmo_content, AssertVmoContentError};

        let expected = $expected;
        match assert_vmo_content($vmo, expected) {
            Ok(()) => (),
            Err(AssertVmoContentError::GetContentSizeFailed(status)) => {
                panic!("`vmo.get_content_size()` failed: {}", status)
            }
            Err(AssertVmoContentError::VmoReadFailed(status)) => {
                panic!("`vmo.read(&mut buffer, 0)` failed: {}", status)
            }
            Err(AssertVmoContentError::UnexpectedContent(buffer)) => panic!(
                "Unexpected content:\n\
                 Expected: {:x?}\n\
                 Actual:   {:x?}\n\
                 Expected as UTF-8 lossy: {:?}\n\
                 Actual as UTF-8 lossy:   {:?}",
                expected,
                &buffer,
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(&buffer),
            ),
        }
    }};
}
