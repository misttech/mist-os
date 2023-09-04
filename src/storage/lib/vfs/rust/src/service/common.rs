// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Code shared between several modules of the service implementation.

use {fidl_fuchsia_io as fio, fuchsia_zircon::Status};

/// Validate that the requested flags for a new connection are valid.  It is a bit tricky as
/// depending on the presence of the `OPEN_FLAG_NODE_REFERENCE` flag we are effectively validating
/// two different cases: with `OPEN_FLAG_NODE_REFERENCE` the connection will be attached to the
/// service node itself, and without `OPEN_FLAG_NODE_REFERENCE` the connection will be forwarded to
/// the backing service.
///
/// `new_connection_validate_flags` will preserve `OPEN_FLAG_NODE_REFERENCE` to make it easier for
/// the caller to distinguish these two cases.
///
/// On success, returns the validated and cleaned flags.  On failure, it returns a [`Status`]
/// indicating the problem.
///
/// Changing this function can be dangerous!  Flags operations may have security implications.
pub fn new_connection_validate_flags(mut flags: fio::OpenFlags) -> Result<fio::OpenFlags, Status> {
    // A service is not a directory.
    flags &= !fio::OpenFlags::NOT_DIRECTORY;

    if flags.intersects(fio::OpenFlags::DIRECTORY) {
        return Err(Status::NOT_DIR);
    }

    if flags.intersects(fio::OpenFlags::NODE_REFERENCE) {
        if flags.intersects(!fio::OPEN_FLAGS_ALLOWED_WITH_NODE_REFERENCE) {
            return Err(Status::INVALID_ARGS);
        }
        return Ok(flags);
    }

    // Anything else is also not allowed.
    if flags.intersects(!fio::OpenFlags::DESCRIBE) {
        return Err(Status::INVALID_ARGS);
    }

    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::new_connection_validate_flags;

    use {fidl_fuchsia_io as fio, fuchsia_zircon::Status};

    /// Assertion for when `new_connection_validate_flags` should succeed => `ncvf_ok`.
    #[track_caller]
    fn ncvf_ok(flags: fio::OpenFlags, expected_new_flags: fio::OpenFlags) {
        match new_connection_validate_flags(flags) {
            Ok(new_flags) => assert_eq!(
                expected_new_flags, new_flags,
                "new_connection_validate_flags returned unexpected set of flags.\n\
                 Expected: {:X}\n\
                 Actual: {:X}",
                expected_new_flags, new_flags
            ),
            Err(status) => panic!("new_connection_validate_flags failed.  Status: {}", status),
        }
    }

    /// Assertion for when `new_connection_validate_flags` should fail => `ncvf_err`.
    #[track_caller]
    fn ncvf_err(flags: fio::OpenFlags, expected_status: Status) {
        match new_connection_validate_flags(flags) {
            Ok(new_flags) => panic!(
                "new_connection_validate_flags should have failed.\n\
                 Got new flags: {:X}",
                new_flags
            ),
            Err(status) => assert_eq!(expected_status, status),
        }
    }

    #[test]
    fn node_reference_basic() {
        // OPEN_FLAG_NODE_REFERENCE is preserved.
        ncvf_ok(fio::OpenFlags::NODE_REFERENCE, fio::OpenFlags::NODE_REFERENCE);

        // OPEN_FLAG_DESCRIBE is preserved.
        ncvf_ok(
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE,
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE,
        );

        ncvf_ok(
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::NOT_DIRECTORY,
            fio::OpenFlags::NODE_REFERENCE,
        );
        ncvf_err(fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DIRECTORY, Status::NOT_DIR);

        // Access flags are forbidden.
        ncvf_err(
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::RIGHT_READABLE,
            Status::INVALID_ARGS,
        );
        ncvf_err(
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::RIGHT_WRITABLE,
            Status::INVALID_ARGS,
        );
    }

    #[test]
    fn service_basic() {
        // Access flags are forbidden.
        ncvf_err(fio::OpenFlags::RIGHT_READABLE, Status::INVALID_ARGS);
        ncvf_err(fio::OpenFlags::RIGHT_WRITABLE, Status::INVALID_ARGS);
        ncvf_ok(fio::OpenFlags::empty(), fio::OpenFlags::empty());

        // OPEN_FLAG_DESCRIBE is allowed.
        ncvf_ok(fio::OpenFlags::DESCRIBE, fio::OpenFlags::DESCRIBE);

        ncvf_ok(fio::OpenFlags::NOT_DIRECTORY, fio::OpenFlags::empty());
        ncvf_err(fio::OpenFlags::DIRECTORY, Status::NOT_DIR);
    }

    #[test]
    fn node_reference_posix() {
        // OPEN_FLAG_POSIX_* is forbidden.
        ncvf_err(
            fio::OpenFlags::NODE_REFERENCE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            Status::INVALID_ARGS,
        );
        ncvf_err(
            fio::OpenFlags::NODE_REFERENCE
                | fio::OpenFlags::DESCRIBE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            Status::INVALID_ARGS,
        );
        ncvf_err(
            fio::OpenFlags::NODE_REFERENCE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            Status::INVALID_ARGS,
        );
    }

    #[test]
    fn service_posix() {
        // OPEN_FLAG_POSIX_* is forbidden.
        ncvf_err(
            fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE,
            Status::INVALID_ARGS,
        );
        ncvf_err(
            fio::OpenFlags::DESCRIBE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            Status::INVALID_ARGS,
        );
    }
}
