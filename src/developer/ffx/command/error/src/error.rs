// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use errors::FfxError;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use errors::{ffx_error, ffx_error_with_code, IntoExitCode};
    use std::io::{Cursor, Write};

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

    #[test]
    fn test_from_ok_early_exit() {
        let command = ["testing", "--help"];
        let output = "stuff!".to_owned();
        let status = Ok(());
        let code = 0;

        let early_exit = argh::EarlyExit { output: output.clone(), status };
        let err = Error::from_early_exit(&command, early_exit);
        assert_eq!(err.exit_code(), code);
        assert_matches!(err, Error::Help { command: error_command, output: error_output, code: error_code } if error_command == command && error_output == output && error_code == code);
    }

    #[test]
    fn test_from_error_early_exit() {
        let command = ["testing", "bad", "command"];
        let output = "stuff!".to_owned();
        let status = Err(());
        let code = 1;

        let early_exit = argh::EarlyExit { output: output.clone(), status };
        let err = Error::from_early_exit(&command, early_exit);
        assert_eq!(err.exit_code(), code);
        assert_matches!(err, Error::Config(err) if format!("{err}") == output);
    }
}
