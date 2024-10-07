// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_types::NamespacePath;
use namespace::Namespace;

use zx::Status;

use fdf::DispatcherRef;
use fidl_fuchsia_driver_framework::DriverStartArgs;

/// The context arguments passed to the driver in its start arguments.
pub struct DriverContext {
    /// A reference to the root [`fdf::Dispatcher`] for this driver.
    pub root_dispatcher: DispatcherRef<'static>,
    /// The original [`DriverStartArgs`] passed in as start arguments, minus any parts that were
    /// used to construct other elements of [`Self`].
    pub start_args: DriverStartArgs,
    /// The incoming namespace constructed from [`DriverStartArgs::incoming`]. Since producing this
    /// consumes the incoming namespace from [`Self::start_args`], that will always be [`None`].
    pub incoming: Namespace,
    #[doc(hidden)]
    _private: (),
}

impl DriverContext {
    pub(crate) fn new(
        root_dispatcher: DispatcherRef<'static>,
        mut start_args: DriverStartArgs,
    ) -> Result<Self, Status> {
        let incoming = start_args
            .incoming
            .take()
            .unwrap_or_else(|| vec![])
            .try_into()
            .map_err(|_| Status::INVALID_ARGS)?;
        Ok(DriverContext { root_dispatcher, start_args, incoming, _private: () })
    }

    pub(crate) fn start_logging(&self, driver_name: &str) -> Result<(), Status> {
        let Some(svc_path) =
            self.incoming.get(&NamespacePath::new("/svc").expect("/svc is a valid path"))
        else {
            // just bail if there is no service path
            eprintln!("Warning: no service path offered to driver, proceeding without logging initialized");
            return Ok(());
        };
        let log_proxy = fuchsia_component::client::connect_to_protocol_at_dir_root::<
            fidl_fuchsia_logger::LogSinkMarker,
        >(svc_path)
        .map_err(|err| {
            eprintln!("Error connecting to log sink proxy at driver startup: {err}");
            Status::INVALID_ARGS
        })?;

        diagnostics_log::initialize(
            diagnostics_log::PublishOptions::default()
                .use_log_sink(log_proxy)
                .tags(&["driver", driver_name]),
        )
        .map_err(|err| {
            eprintln!("Error initializing logging at driver startup: {err}");
            Status::INVALID_ARGS
        })
    }
}
