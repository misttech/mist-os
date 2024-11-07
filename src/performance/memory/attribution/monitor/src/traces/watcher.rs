// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::sync::OnceLock;
use tokio::sync::watch::{self, Receiver, Sender};
use tracing::debug;

// Use a singleton so that `trace_provider_create_with_fdio` is used only once, and the
// `c_callback` can interact with a `TraceObserver` instance.
static TRACE_SINGLETON: OnceLock<TraceSingleton> = OnceLock::new();

pub trait WatcherInterface {
    fn recv(&mut self) -> impl std::future::Future<Output = ()> + Send;
}
/// Detects changes in the tracing system state.
pub struct Watcher {
    receiver: Receiver<()>,
}

impl Watcher {
    pub fn new(receiver: Receiver<()>) -> Watcher {
        Watcher { receiver }
    }

    /// Returns when one of more changes happened in the tracing system configuration.
    pub async fn recv(&mut self) {
        match self.receiver.changed().await {
            Ok(_) => {} // One or more event occurred.
            Err(_) => panic!("Dropping the sender should never happen"),
        }
    }
}
/// Setup a new observer and returns a Watcher object.
/// Initializes the tracing system observation on first call.
pub fn subscribe() -> Watcher {
    let receiver = TRACE_SINGLETON.get_or_init(|| TraceSingleton::new()).sender.subscribe();
    Watcher::new(receiver)
}

// Holds synchronization primitives, and guarantees that the trace system is initialized only once.
struct TraceSingleton {
    // Receives notifications when the trace state or set of enabled
    // categories changes.
    sender: Sender<()>,
}

extern "C" fn c_callback() {
    TRACE_SINGLETON
        .get()
        .expect("Unexpected callback called before observer initialization")
        .sender
        .send(())
        .ok(); // Error means there is no receiver connected, and can be ignored.
}

impl TraceSingleton {
    fn new() -> TraceSingleton {
        debug!("Register trace provider");
        fuchsia_trace_provider::trace_provider_create_with_fdio();
        debug!("Setup trace observer callback");
        fuchsia_trace_observer::start_trace_observer(c_callback);
        let (sender, _) = watch::channel(());
        TraceSingleton { sender }
    }
}
