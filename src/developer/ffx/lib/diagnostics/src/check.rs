// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;
use std::pin::Pin;

pub type CheckResult<T> = Result<T, anyhow::Error>;

pub type CheckFut<'a, T> = Pin<Box<dyn Future<Output = CheckResult<T>> + 'a>>;

pub trait Check {
    /// The input for the next check in the check stream.
    type Input;

    /// The output for the next check to take as input.
    type Output;

    /// The type of the writer used in each check. Defaults to a vector of bytes for testing, but
    /// this should typically be used with subtools, so one would pass the tool's `::Writer` to
    /// this.
    type Writer;

    /// Optional write before the check is run.
    fn write_preamble(
        &self,
        _input: &Self::Input,
        _writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Optional write on success of check.
    fn on_success(
        &self,
        _output: &Self::Output,
        _writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Required: the actual check being implemented.
    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        writer: &'a mut Self::Writer,
    ) -> CheckFut<'a, Self::Output>;

    /// Auto-implemented. Runs a check while writing a preamble. Returns a tuple of the main output
    /// and the reference to the writer being used.
    ///
    /// Allows for chaining checks together.
    fn check_with_output<'a>(
        mut self,
        input: Self::Input,
        writer: &'a mut Self::Writer,
    ) -> CheckFut<'a, (Self::Output, &'a mut Self::Writer)>
    where
        Self::Writer: Sized,
        Self: Sized + 'a,
    {
        Box::pin(async move {
            self.write_preamble(&input, writer)?;
            let result = (self.check(input, writer).await?, writer);
            self.on_success(&result.0, result.1)?;
            Ok(result)
        })
    }
}
