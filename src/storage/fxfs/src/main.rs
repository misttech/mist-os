// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_async as fasync;
use fuchsia_component::server::MissingStartupHandle;
use fuchsia_runtime::HandleType;
use fxfs::log::*;
use fxfs::serialized_types::LATEST_VERSION;
use fxfs_platform::component::Component;

#[fasync::run(6)]
async fn main() -> Result<(), Error> {
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default())?;
    let _inspect_server_task = inspect_runtime::publish(
        fuchsia_inspect::component::inspector(),
        inspect_runtime::PublishOptions::default().send_vmo_preference(
            inspect_runtime::TreeServerSendPreference::frozen_or(
                inspect_runtime::TreeServerSendPreference::DeepCopy,
            ),
        ),
    );

    fuchsia_trace_provider::trace_provider_create_with_fdio();

    info!(version:% = LATEST_VERSION; "Started");

    Component::new()
        .run(
            fuchsia_runtime::take_startup_handle(HandleType::DirectoryRequest.into())
                .ok_or(MissingStartupHandle)?
                .into(),
            fuchsia_runtime::take_startup_handle(HandleType::Lifecycle.into()).map(|h| h.into()),
        )
        .await
}
