// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TestEnv;
use futures::channel::mpsc;
use futures::{StreamExt, TryStreamExt};
use log::*;
use {fidl_fuchsia_hardware_power_statecontrol as fpower, fuchsia_async as fasync};

/// Convenience type for interacting with the Power Manager's RebootMethodsWatcher service.
// TODO(https://fxbug.dev/385742868): Delete this client impl once the Watcher
// API is deleted.
pub struct DeprecatedRebootWatcherClient {
    _watcher_task: fasync::Task<()>,
    reboot_reason_receiver: mpsc::Receiver<fpower::RebootReason>,
}

impl DeprecatedRebootWatcherClient {
    pub async fn new(test_env: &TestEnv) -> Self {
        let (mut reboot_reason_sender, reboot_reason_receiver) = mpsc::channel(1);

        // Create a new watcher proxy/stream and register the proxy end with Power Manager
        let watcher_register_proxy =
            test_env.connect_to_protocol::<fpower::RebootMethodsWatcherRegisterMarker>();
        let (watcher_client, mut watcher_request_stream) =
            fidl::endpoints::create_request_stream::<fpower::RebootMethodsWatcherMarker>();
        watcher_register_proxy
            .register_with_ack(watcher_client)
            .await
            .expect("Failed to register reboot watcher");

        let _watcher_task = fasync::Task::local(async move {
            while let Some(fpower::RebootMethodsWatcherRequest::OnReboot { reason, responder }) =
                watcher_request_stream.try_next().await.unwrap()
            {
                info!("Received reboot reason: {:?}", reason);
                let _ = responder.send();
                reboot_reason_sender.try_send(reason).expect("Failed to notify reboot reason");
            }
        });

        Self { _watcher_task, reboot_reason_receiver }
    }

    /// Returns the next reboot reason that the reboot watcher has received, or hangs until one is
    /// received.
    pub async fn get_reboot_reason(&mut self) -> fpower::RebootReason {
        self.reboot_reason_receiver.next().await.expect("Failed to wait for reboot reason")
    }
}

/// Convenience type for interacting with the Power Manager's RebootWatcher service.
pub struct RebootWatcherClient {
    _watcher_task: fasync::Task<()>,
    reboot_options_receiver: mpsc::Receiver<fpower::RebootOptions>,
}

impl RebootWatcherClient {
    pub async fn new(test_env: &TestEnv) -> Self {
        let (mut reboot_options_sender, reboot_options_receiver) = mpsc::channel(1);

        // Create a new watcher proxy/stream and register the proxy end with Power Manager
        let watcher_register_proxy =
            test_env.connect_to_protocol::<fpower::RebootMethodsWatcherRegisterMarker>();
        let (watcher_client, mut watcher_request_stream) =
            fidl::endpoints::create_request_stream::<fpower::RebootWatcherMarker>();
        watcher_register_proxy
            .register_watcher(watcher_client)
            .await
            .expect("Failed to register reboot watcher");

        let _watcher_task = fasync::Task::local(async move {
            while let Some(fpower::RebootWatcherRequest::OnReboot { options, responder }) =
                watcher_request_stream.try_next().await.unwrap()
            {
                info!("Received reboot options: {:?}", options);
                let _ = responder.send();
                reboot_options_sender.try_send(options).expect("Failed to notify reboot reason");
            }
        });

        Self { _watcher_task, reboot_options_receiver }
    }

    /// Returns the next reboot reason that the reboot watcher has received, or hangs until one is
    /// received.
    pub async fn get_reboot_options(&mut self) -> fpower::RebootOptions {
        self.reboot_options_receiver.next().await.expect("Failed to wait for reboot reason")
    }
}
