// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Error;
use anyhow::Context;
use errors::{FfxError, IntoExitCode, ResultExt};
use std::fmt::Display;

/// Adds helpers to result types to produce useful error messages to the user from
/// the ffx frontend (through [`crate::Error`])
pub trait FfxContext<T, E> {
    /// Make this error into a BUG check that will display to the user as an error that
    /// shouldn't happen.
    fn bug(self) -> Result<T, Error>;

    /// Make this error into a BUG check that will display to the user as an error that
    /// shouldn't happen, with the added context.
    fn bug_context<C: Display + Send + Sync + 'static>(self, context: C) -> Result<T, Error>;

    /// Make this error into a BUG check that will display to the user as an error that
    /// shouldn't happen, with the added context returned by the closure `f`.
    fn with_bug_context<C: Display + Send + Sync + 'static>(
        self,
        f: impl FnOnce() -> C,
    ) -> Result<T, Error>;

    /// Make this error into a displayed user error, with the added context for display to the user.
    /// Use this for errors that happen in the normal course of execution, like files not being found.
    fn user_message<C: Display + Send + Sync + 'static>(self, context: C) -> Result<T, Error>;

    /// Make this error into a displayed user error, with the added context for display to the user.
    /// Use this for errors that happen in the normal course of execution, like files not being found.
    fn with_user_message<C: Display + Send + Sync + 'static>(
        self,
        f: impl FnOnce() -> C,
    ) -> Result<T, Error>;
}

impl<T, E> FfxContext<T, E> for Result<T, E>
where
    Self: anyhow::Context<T, E>,
    E: Into<anyhow::Error>,
{
    fn bug(self) -> Result<T, Error> {
        self.map_err(|e| Error::Unexpected(e.into()))
    }

    fn bug_context<C: Display + Send + Sync + 'static>(self, context: C) -> Result<T, Error> {
        self.context(context).map_err(Error::Unexpected)
    }

    fn with_bug_context<C: Display + Send + Sync + 'static>(
        self,
        f: impl FnOnce() -> C,
    ) -> Result<T, Error> {
        self.with_context(f).map_err(Error::Unexpected)
    }

    fn user_message<C: Display + Send + Sync + 'static>(self, context: C) -> Result<T, Error> {
        self.context(context).map_err(Error::User)
    }

    fn with_user_message<C: Display + Send + Sync + 'static>(
        self,
        f: impl FnOnce() -> C,
    ) -> Result<T, Error> {
        self.with_context(f).map_err(Error::User)
    }
}

impl ResultExt for Error {
    fn ffx_error<'a>(&'a self) -> Option<&'a FfxError> {
        match self {
            Error::User(err) => err.downcast_ref(),
            _ => None,
        }
    }
}
impl IntoExitCode for Error {
    fn exit_code(&self) -> i32 {
        use Error::*;
        match self {
            Help { code, .. } => *code,
            Unexpected(err) | User(err) | Config(err) => {
                err.ffx_error().map(FfxError::exit_code).unwrap_or(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use anyhow::anyhow;
    use assert_matches::assert_matches;

    #[test]
    fn error_context_helpers() {
        assert_matches!(
            anyhow::Result::<()>::Err(anyhow!(ERR_STR)).bug(),
            Err(Error::Unexpected(_)),
            "anyhow.bug() should be a bugcheck error"
        );
        assert_matches!(
            anyhow::Result::<()>::Err(anyhow!(ERR_STR)).bug_context("boom"),
            Err(Error::Unexpected(_)),
            "anyhow.bug_context() should be a bugcheck error"
        );
        assert_matches!(
            anyhow::Result::<()>::Err(anyhow!(ERR_STR)).with_bug_context(|| "boom"),
            Err(Error::Unexpected(_)),
            "anyhow.bug_context() should be a bugcheck error"
        );
        assert_matches!(anyhow::Result::<()>::Err(anyhow!(ERR_STR)).bug_context(FfxError::TestingError), Err(Error::Unexpected(_)), "anyhow.bug_context() should create a bugcheck error even if given an ffx error (magic reduction)");
        assert_matches!(
            anyhow::Result::<()>::Err(anyhow!(ERR_STR)).user_message("boom"),
            Err(Error::User(_)),
            "anyhow.user_message() should be a user error"
        );
        assert_matches!(
            anyhow::Result::<()>::Err(anyhow!(ERR_STR)).with_user_message(|| "boom"),
            Err(Error::User(_)),
            "anyhow.with_user_message() should be a user error"
        );
        assert_matches!(anyhow::Result::<()>::Err(anyhow!(ERR_STR)).with_user_message(|| FfxError::TestingError).ffx_error(), Some(FfxError::TestingError), "anyhow.with_user_message should be a user error that properly extracts to the ffx error.");
    }
}
