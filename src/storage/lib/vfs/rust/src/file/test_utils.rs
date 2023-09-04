// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common utilities used by pseudo-file related tests.

use crate::{
    directory::entry::DirectoryEntry,
    test_utils::run::{self, AsyncServerClientTestParams},
};

use {
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{Status, Vmo},
    futures::Future,
    std::{convert::Infallible, sync::Arc},
};

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
    /// Failure returned from the `vmo.read()` call.
    VmoReadFailed(Status),
    /// Expected content and the actual VMO content did not match.
    UnexpectedContent(Vec<u8>),
}

/// Reads the VMO content and matches it against the expectation.
pub fn assert_vmo_content(vmo: &Vmo, expected: &[u8]) -> Result<(), AssertVmoContentError> {
    let mut buffer = Vec::with_capacity(expected.len());
    buffer.resize(expected.len(), 0);
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

/// Possible errors for the [`report_invalid_vmo_content()`] function.
pub enum ReportInvalidVmoContentError {
    /// Failure returned from the `vmo.read()` call.
    VmoReadFailed(Status),
}

/// A helper function to panic with a message that includes the VMO content and a specified
/// `context` message.
pub fn report_invalid_vmo_content(
    vmo: &Vmo,
    context: &str,
) -> Result<Infallible, ReportInvalidVmoContentError> {
    // For debugging purposes we print the first 100 bytes.  This is an arbitrary choice.
    let mut buffer = Vec::with_capacity(100);
    buffer.resize(100, 0);
    vmo.read(&mut buffer, 0).map_err(ReportInvalidVmoContentError::VmoReadFailed)?;
    panic!(
        "{}.  Content:\n\
         {:x?}",
        context, buffer
    );
}

/// Wraps a [`report_invalid_vmo_content()`] call, panicking with a descriptive error message for
/// any `Err` return values.
#[macro_export]
macro_rules! report_invalid_vmo_content {
    ($vmo:expr, $context:expr) => {{
        use $crate::file::test_utils::{report_invalid_vmo_content, ReportInvalidVmoContentError};

        match report_invalid_vmo_content($vmo, $context).void_unwrap_err() {
            ReportInvalidVmoContentError::VmoReadFailed(status) => {
                panic!("`vmo.read(&mut buffer, 0)` failed: {}", status)
            }
        }
    }};
}
