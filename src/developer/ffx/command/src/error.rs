// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use errors::{FfxError, IntoExitCode, ResultExt};
use std::fmt::Display;

/// A top level error type for ffx tool results
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// An error that qualifies as a bugcheck
    Unexpected(#[source] anyhow::Error),
    /// A known kind of error that can be reported usefully to the user
    User(#[source] anyhow::Error),
    /// An early-exit that should result in outputting help to the user (like [`argh::EarlyExit`]),
    /// but is not itself an error in any meaningful sense.
    Help {
        /// The command name (argv[0..]) that should be used in supplemental help output
        command: Vec<String>,
        /// The text to output to the user
        output: String,
        /// The exit status
        code: i32,
    },
    /// Something failed before ffx's configuration could be loaded (like an
    /// invalid argument, a failure to read an env config file, etc).
    ///
    /// Errors of this type should include any information the user might need
    /// to recover from the issue, because it will not advise the user to look
    /// in the log files or anything like that.
    Config(#[source] anyhow::Error),
}

/// Writes a detailed description of an anyhow error to the formatter
fn write_detailed(f: &mut std::fmt::Formatter<'_>, error: &anyhow::Error) -> std::fmt::Result {
    write!(f, "Error: {}", error)?;
    for (i, e) in error.chain().skip(1).enumerate() {
        write!(f, "\n  {: >3}.  {}", i + 1, e)?;
    }
    Ok(())
}

const BUG_LINE: &str = "BUG: An internal command error occurred.";
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unexpected(error) => {
                writeln!(f, "{BUG_LINE}")?;
                write_detailed(f, error)
            }
            Self::User(error) | Self::Config(error) => write!(f, "{error}"),
            Self::Help { output, .. } => write!(f, "{output}"),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        // this is just a compatibility shim to extract information out of the way
        // we've traditionally divided user and unexpected errors.
        match error.downcast::<FfxError>() {
            Ok(err) => Self::User(err.into()),
            Err(err) => Self::Unexpected(err),
        }
    }
}

impl From<FfxError> for Error {
    fn from(error: FfxError) -> Self {
        Error::User(error.into())
    }
}

impl Error {
    /// Map an argh early exit to our kind of error
    pub fn from_early_exit(command: &[impl AsRef<str>], early_exit: argh::EarlyExit) -> Self {
        let command = Vec::from_iter(command.iter().map(|s| s.as_ref().to_owned()));
        let output = early_exit.output;
        // if argh's early_exit status is Ok() that means it's printing help because
        // of a `--help` argument or `help` as a subcommand was passed. Otherwise
        // it's just an error parsing the arguments. So only map `status: Ok(())`
        // as help output.
        match early_exit.status {
            Ok(_) => Error::Help { command, output, code: 0 },
            Err(_) => Error::Config(anyhow::anyhow!("{}", output)),
        }
    }

    /// Get the exit code this error should correspond to if it bubbles up to `main()`
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::User(err) => {
                if let Some(FfxError::Error(_, code)) = err.downcast_ref() {
                    *code
                } else {
                    1
                }
            }
            Error::Help { code, .. } => *code,
            _ => 1,
        }
    }
}

/// A convenience Result type
pub type Result<T, E = crate::Error> = core::result::Result<T, E>;

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
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use errors::{ffx_error, ffx_error_with_code};
    use std::io::{Cursor, Write};

    const FFX_STR: &str = "I am an ffx error";
    const ERR_STR: &str = "I am not an ffx error";

    #[test]
    fn test_write_result_ffx_error() {
        let err = Error::from(ffx_error!(FFX_STR));
        let mut cursor = Cursor::new(Vec::new());

        assert_matches!(write!(&mut cursor, "{err}"), Ok(_));

        assert!(String::from_utf8(cursor.into_inner()).unwrap().contains(FFX_STR));
    }

    #[test]
    fn into_error_from_arbitrary_is_unexpected() {
        let err = anyhow!(ERR_STR);
        assert_matches!(
            Error::from(err),
            Error::Unexpected(_),
            "an arbitrary anyhow error should convert to an 'unexpected' bug check error"
        );
    }

    #[test]
    fn into_error_from_ffx_error_is_user_error() {
        let err = FfxError::Error(anyhow!(FFX_STR), 1);
        assert_matches!(
            Error::from(err),
            Error::User(_),
            "an arbitrary anyhow error should convert to a 'user' error"
        );
    }

    #[test]
    fn into_error_from_contextualized_ffx_error_prints_original_error() {
        let err = Error::from(anyhow::anyhow!(errors::ffx_error!(FFX_STR)).context("boom"));
        assert_eq!(
            &format!("{err}"),
            FFX_STR,
            "an anyhow error with context should print the original error, not the context, when stringified."
        );
    }

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

    #[test]
    fn test_write_result_arbitrary_error() {
        let err = Error::from(anyhow!(ERR_STR));
        let mut cursor = Cursor::new(Vec::new());

        assert_matches!(write!(&mut cursor, "{err}"), Ok(_));

        let err_str = String::from_utf8(cursor.into_inner()).unwrap();
        assert!(err_str.contains(BUG_LINE));
        assert!(err_str.contains(ERR_STR));
    }

    #[test]
    fn test_result_ext_exit_code_ffx_error() {
        let err = Result::<()>::Err(Error::from(ffx_error_with_code!(42, FFX_STR)));
        assert_eq!(err.exit_code(), 42);
    }
}
