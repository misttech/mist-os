// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

/// Result type that uses our error type as the default.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A stack frame for errors which produce a backtrace.
#[derive(Clone, Default)]
struct Frame {
    function: Option<String>,
    file: Option<String>,
    span: Option<Span>,
}

/// A source code position for error messages.
#[derive(Copy, Clone, Default)]
struct Span {
    line: usize,
    col: Option<(usize, usize)>,
}

/// Error type returned by failures when running playground commands.
#[derive(Clone)]
pub struct Error {
    stack: Vec<Frame>,
    pub source: Arc<anyhow::Error>,
}

impl<'a> std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, f)?;

        if f.alternate() {
            for (i, frame) in self.stack.iter().enumerate() {
                write!(f, "\n")?;
                write!(f, " #{:3} ", i)?;
                if let Some(function) = &frame.function {
                    write!(f, "in {}", function)?;
                } else {
                    write!(f, "at toplevel")?;
                }

                if frame.file.is_some() || frame.span.is_some() {
                    write!(f, " (")?;
                }

                if let Some(file) = &frame.file {
                    write!(f, "{}", file)?;

                    if frame.span.is_some() {
                        write!(f, " ")?;
                    }
                }

                if let Some(span) = &frame.span {
                    write!(f, "line {}", span.line)?;

                    if let Some((start, end)) = &span.col {
                        if start == end {
                            write!(f, " column {}", start)?;
                        } else {
                            write!(f, " column {}-{}", start, end)?;
                        }
                    }
                }

                if frame.file.is_some() || frame.span.is_some() {
                    write!(f, ")")?;
                }
            }
        }
        Ok(())
    }
}

impl<'a> std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self, f)
    }
}

impl<T> From<T> for Error
where
    T: Into<anyhow::Error>,
{
    fn from(source: T) -> Error {
        Error { stack: Vec::new(), source: Arc::new(source.into()) }
    }
}

impl Error {
    /// Add a stack frame to this error.
    pub fn in_func(&mut self, func: &str) {
        if let Some(frame) = self.stack.last_mut().filter(|x| x.function.is_none()) {
            frame.function = Some(func.to_owned())
        } else {
            self.stack.push(Frame { function: Some(func.to_owned()), file: None, span: None })
        }
    }

    /// Annotate an error with the file, line, and column (start and end) where it occurred.
    pub fn at(&mut self, file: Option<String>, line: usize, col: Option<(usize, usize)>) {
        self.stack.push(Frame { function: None, file, span: Some(Span { line, col }) })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_few_deep() {
        let error = anyhow::anyhow!("Test error");
        let mut error = Error::from(error);
        error.in_func("foo");
        error.in_func("bar");

        assert_eq!("Test error\n #  0 in foo\n #  1 in bar", format!("{error:#?}"));
    }

    #[test]
    fn located() {
        let error = anyhow::anyhow!("Test error");
        let mut error = Error::from(error);
        error.at(Some("my_script.script".to_owned()), 23, None);
        error.in_func("foo");
        error.at(None, 45, Some((5, 7)));
        error.in_func("bar");
        error.at(Some("your_script.script".to_owned()), 5, Some((1, 1)));
        error.in_func("baz");

        assert_eq!(
            "Test error\n \
                    #  0 in foo (my_script.script line 23)\n \
                    #  1 in bar (line 45 column 5-7)\n \
                    #  2 in baz (your_script.script line 5 column 1)",
            format!("{error:#?}")
        );
    }
}
