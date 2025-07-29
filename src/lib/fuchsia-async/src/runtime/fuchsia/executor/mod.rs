// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// A future which can be used by multiple threads at once.
mod atomic_future;
mod common;
mod local;
mod packets;
pub mod scope;
mod send;
mod time;

pub use atomic_future::spawnable_future::SpawnableFuture;
pub use common::EHandle;
pub(crate) use common::Executor;
pub use local::{LocalExecutor, LocalExecutorBuilder, TestExecutor, TestExecutorBuilder};
pub use packets::{PacketReceiver, RawReceiverRegistration, ReceiverRegistration};
pub use send::{SendExecutor, SendExecutorBuilder};
pub use time::{BootInstant, MonotonicDuration, MonotonicInstant};
