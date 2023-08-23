// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fuchsia_async as fasync,
    fuchsia_component::server::MissingStartupHandle,
    fuchsia_runtime::HandleType,
    fxfs::{log::*, serialized_types::LATEST_VERSION},
    fxfs_platform::component::Component,
};

#[fasync::run(6)]
async fn main() -> Result<(), Error> {
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default())?;

    #[cfg(feature = "tracing")]
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    info!(version = %LATEST_VERSION, "Started");

    Component::new()
        .run(
            fuchsia_runtime::take_startup_handle(HandleType::DirectoryRequest.into())
                .ok_or(MissingStartupHandle)?
                .into(),
            fuchsia_runtime::take_startup_handle(HandleType::Lifecycle.into()).map(|h| h.into()),
        )
        .await
}
