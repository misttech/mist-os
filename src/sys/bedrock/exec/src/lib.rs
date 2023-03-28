// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The `exec` library provides common tools and interfaces that allow users to run and manage
//! software on Fuchsia at various levels of abstraction, from Zircon processes to components.
//!
//! # Lifecycle Traits
//!
//! The `Start` and `Stop` traits represent the fundamental `exec` state machine. Types that
//! implement `Start` can be started, moving them into a "started" state. This returns a
//! typestate that implements the `Stop` trait, used to transition into a "stopped" state.
//!
//! This structure has the following properties:
//!
//! * Any type that can be started, can be stopped.
//! * Types may be stopped implicitly (for example, due to a crash)
//! * A type that implements `Stop`, but not `Start`, cannot be restarted.
//!
//! # Built-in Implementations
//!
//! This library contains a few built-in implementations of the lifecycle traits:
//!
//! * `Elf`: Runs ELF binaries, possibly from a package.
//! * `BuiltProcess`: Runs a Zircon process given a `process_builder::BuiltProcess`.
//!
//! # Examples
//!
//! A type that can be started once:
//!
//! ```
//! use exec::*;
//! use async_trait::async_trait;
//!
//! struct SingleRun {}
//!
//! #[async_trait]
//! impl Start for SingleRun {
//!     type Error = ();
//!     type Stop = Execution;
//!
//!     async fn start(self) -> Result<Self::Stop, Self::Error> {
//!         Ok(Execution {})
//!     }
//! }
//!
//! struct Execution {}
//!
//! #[async_trait]
//! impl Stop for Execution {
//!     type Error = ();
//!
//!     async fn stop(&self) -> Result<(), Self::Error> {
//!         Ok(())
//!     }
//! }
//! ```
//!
//! A type that can be restarted, implementing both `Start` and `Stop`:
//!
//! ```
//! use exec::*;
//! use async_trait::async_trait;
//!
//! struct Restartable {}
//!
//! #[async_trait]
//! impl Start for Restartable {
//!     type Error = ();
//!     type Stop = Self;
//!
//!     async fn start(self) -> Result<Self::Stop, Self::Error> {
//!         Ok(self)
//!     }
//! }
//!
//! #[async_trait]
//! impl Stop for Restartable {
//!     type Error = ();
//!
//!     async fn stop(&self) -> Result<(), Self::Error> {
//!         Ok(())
//!     }
//! }
//! ```

use {async_trait::async_trait, fuchsia_zircon as zx};

pub mod elf;
pub mod process;

/// A trait for types that can start something, or be started.
#[async_trait]
pub trait Start {
    /// The type returned when starting fails.
    ///
    /// This error type is shared with the `Stop` type, so a single type represents errors
    /// for both starting and stopping.
    type Error;

    /// The "started" typestate.
    type Stop: Stop<Error = Self::Error>;

    /// Performs the start operation.
    async fn start(self) -> Result<Self::Stop, Self::Error>;
}

/// A trait for types that can be stopped.
#[async_trait]
pub trait Stop {
    /// The type returned when starting fails.
    type Error;

    /// Performs the stop operation.
    ///
    /// This does not consume `self` because some implementations may want to allow clients
    /// to access terminal state.
    ///
    /// The type may transition to the stopped state implicitly, without a client calling `stop()`.
    /// This may also create terminal state (e.g. a return code). Hence, `stop()` does not return
    /// any terminal state, only the result the stop operation.
    async fn stop(&mut self) -> Result<(), Self::Error>;
}

/// `Stop`s that exit with some information.
#[async_trait]
pub trait Lifecycle: Stop {
    /// The information returned when the `Stop` exits.
    type Exit;

    /// Waits for the `Stop` to exit and returns its error if any.
    async fn on_exit(&self) -> Result<Self::Exit, Self::Error>;
}

/// `Stop`s that exit with a return code and may fail to observe exit
/// with a Zircon status code.
#[async_trait]
pub trait Program: Lifecycle<Exit = i64, Error = zx::Status> {}
