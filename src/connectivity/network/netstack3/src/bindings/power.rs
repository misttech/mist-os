// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 bindings system power handling.

use async_utils::hanging_get::client::HangingGetStream;
use futures::StreamExt;
use log::{debug, error};
use netstack3_core::device::WeakDeviceId;
use thiserror::Error;

use {fidl_fuchsia_power_broker as fpower_broker, fidl_fuchsia_power_system as fpower_system};

use crate::bindings::devices::{DeviceSpecificInfo, EthernetInfo, PureIpDeviceInfo};
use crate::bindings::{BindingsCtx, Ctx, DeviceIdExt};

/// Handles tx path suspension for devices.
pub(crate) enum TransmitSuspensionHandler {
    Enabled(EnabledTransmitSuspensionHandler),
    Disabled,
}

impl TransmitSuspensionHandler {
    /// Creates a new `TransmitSuspensionHandler` using the ambient
    /// capabilities.
    ///
    /// If `Ctx` is not configured for suspension, creates a
    /// [`TransmitSuspensionHandler::Disabled`] which does not perform any
    /// suspension blocking.
    ///
    /// Similarly if any fatal errors occur during set up, a `Disabled` handler
    /// is returned.
    pub(crate) async fn new(ctx: &Ctx, device: WeakDeviceId<BindingsCtx>) -> Self {
        if !ctx.bindings_ctx().config.suspend_enabled {
            return Self::Disabled;
        }

        match EnabledTransmitSuspensionHandler::new(device).await {
            Ok(enabled) => Self::Enabled(enabled),
            Err(e) => {
                error!(
                    "error creating transmit suspension handler: {e:?}.\n\
                    Proceeding without tx suspension handling."
                );
                Self::Disabled
            }
        }
    }

    /// Waits for a suspension request.
    ///
    /// See [`FatalTransmitSuspensionError`] for proper error handling with this
    /// function.
    pub(crate) async fn wait(
        &mut self,
    ) -> Result<TransmitSuspensionRequest<'_>, FatalTransmitSuspensionError> {
        match self {
            Self::Disabled => futures::future::pending().await,
            Self::Enabled(h) => h.wait().await,
        }
    }

    /// Disables this suspension handler with `err` as a reason.
    pub(crate) fn disable(&mut self, err: FatalTransmitSuspensionError) {
        log::error!("disabling suspension handling after fatal error: {err:?}");
        *self = Self::Disabled;
    }
}

/// An error that can occur while attempting to handle transmit suspension.
///
/// Upon observing this error, callers should disable the
/// [`TransmitSuspensionHandler`] that observed the error.
///
/// This exists as a type to steer users of this API to do the right thing while
/// sidestepping borrow checker limitations around the signature of
/// [`TransmitSuspensionHandler::wait`].
#[derive(Error, Debug)]
pub(crate) enum FatalTransmitSuspensionError {
    #[error("invalid power level {0}")]
    InvalidPowerLevel(fpower_broker::PowerLevel),
    #[error("required level hanging get stream ended unexpectedly")]
    RequiredLevelStreamEnded,
    #[error("{context} FIDL error: {err}")]
    Fidl { context: &'static str, err: fidl::Error },
    #[error("fetching required level: {0:?}")]
    RequiredLevel(fpower_broker::RequiredLevelError),
    #[error("updating current level: {0:?}")]
    CurrentLevel(fpower_broker::CurrentLevelError),
    #[error("adding element: {0:?}")]
    AddElement(fpower_broker::AddElementError),
    #[error("leasing: {0:?}")]
    Lease(fpower_broker::LeaseError),
}

impl From<fpower_broker::RequiredLevelError> for FatalTransmitSuspensionError {
    fn from(value: fpower_broker::RequiredLevelError) -> Self {
        match &value {
            fpower_broker::RequiredLevelError::Internal
            | fpower_broker::RequiredLevelError::NotAuthorized
            | fpower_broker::RequiredLevelError::Unknown
            | fpower_broker::RequiredLevelError::__SourceBreaking { .. } => {
                Self::RequiredLevel(value)
            }
        }
    }
}

impl From<fpower_broker::CurrentLevelError> for FatalTransmitSuspensionError {
    fn from(value: fpower_broker::CurrentLevelError) -> Self {
        match &value {
            fpower_broker::CurrentLevelError::NotAuthorized
            | fpower_broker::CurrentLevelError::__SourceBreaking { .. } => {
                Self::CurrentLevel(value)
            }
        }
    }
}

impl From<fpower_broker::LeaseError> for FatalTransmitSuspensionError {
    fn from(value: fpower_broker::LeaseError) -> Self {
        match &value {
            fpower_broker::LeaseError::Internal
            | fpower_broker::LeaseError::InvalidLevel
            | fpower_broker::LeaseError::NotAuthorized
            | fpower_broker::LeaseError::__SourceBreaking { .. } => Self::Lease(value),
        }
    }
}

impl From<fpower_broker::AddElementError> for FatalTransmitSuspensionError {
    fn from(value: fpower_broker::AddElementError) -> Self {
        match &value {
            fpower_broker::AddElementError::Invalid
            | fpower_broker::AddElementError::NotAuthorized
            | fpower_broker::AddElementError::__SourceBreaking { .. } => Self::AddElement(value),
        }
    }
}

type TransmitPowerLevel = fpower_broker::BinaryPowerLevel;

type RequiredLevelStream =
    HangingGetStream<fpower_broker::RequiredLevelProxy, fpower_broker::RequiredLevelWatchResult>;

pub(crate) struct EnabledTransmitSuspensionHandler {
    // Never used, but we must keep the client end around so that our Power
    // Element goes to the enabled level when the system's requirements are met.
    _lease: fidl::endpoints::ClientEnd<fpower_broker::LeaseControlMarker>,
    // Never used, but is a statement of liveness for our Power Element.
    _element_control: fidl::endpoints::ClientEnd<fpower_broker::ElementControlMarker>,
    current_level_proxy: fpower_broker::CurrentLevelProxy,
    required_level: RequiredLevelStream,
    current_level: TransmitPowerLevel,
    device: WeakDeviceId<BindingsCtx>,
}

impl EnabledTransmitSuspensionHandler {
    async fn new(device: WeakDeviceId<BindingsCtx>) -> Result<Self, FatalTransmitSuspensionError> {
        let topology =
            fuchsia_component::client::connect_to_protocol::<fpower_broker::TopologyMarker>()
                .expect("connect to fuchsia.power.broker.Topology");
        let sag = fuchsia_component::client::connect_to_protocol::<
            fpower_system::ActivityGovernorMarker,
        >()
        .expect("connect to fuchsia.power.system.ActivityGovernor");

        let fpower_system::PowerElements { execution_state, .. } =
            sag.get_power_elements().await.map_err(|err| FatalTransmitSuspensionError::Fidl {
                context: "get SAG power elements",
                err,
            })?;

        // Execution state must always be present.
        let fpower_system::ExecutionState { opportunistic_dependency_token, .. } =
            execution_state.expect("missing execution state power element");
        // Dep token must always be present.
        let dep_token =
            opportunistic_dependency_token.expect("missing opportunistic dependency token");

        // We take an opportunistic dependency on the system's Suspending level.
        // This allows us to perform work when the system is attempting to move
        // from Suspending to Inactive. Which is when we block the tx queues and
        // finalize any ongoing tx traffic.
        let level_dep = fpower_broker::LevelDependency {
            dependency_type: fpower_broker::DependencyType::Opportunistic,
            dependent_level: TransmitPowerLevel::On.into_primitive(),
            requires_token: dep_token,
            requires_level_by_preference: vec![
                fpower_system::ExecutionStateLevel::Suspending.into_primitive()
            ],
        };

        let (mut current_level_proxy, current_level_server_end) =
            fidl::endpoints::create_proxy::<fpower_broker::CurrentLevelMarker>();
        let (required_level, required_level_server_end) =
            fidl::endpoints::create_proxy::<fpower_broker::RequiredLevelMarker>();
        let (lessor, lessor_server_end) =
            fidl::endpoints::create_proxy::<fpower_broker::LessorMarker>();
        let (element_control, element_control_server_end) =
            fidl::endpoints::create_endpoints::<fpower_broker::ElementControlMarker>();

        let name = device.bindings_id();
        let element_schema = fpower_broker::ElementSchema {
            element_name: Some(format!("netstack::tx::{name}")),
            initial_current_level: Some(TransmitPowerLevel::Off.into_primitive()),
            valid_levels: Some(vec![
                TransmitPowerLevel::Off.into_primitive(),
                TransmitPowerLevel::On.into_primitive(),
            ]),
            dependencies: Some(vec![level_dep]),
            level_control_channels: Some(fpower_broker::LevelControlChannels {
                current: current_level_server_end,
                required: required_level_server_end,
            }),
            lessor_channel: Some(lessor_server_end),
            element_control: Some(element_control_server_end),
            ..Default::default()
        };

        topology.add_element(element_schema).await.map_err(|err| {
            FatalTransmitSuspensionError::Fidl { context: "adding element", err }
        })??;

        let lease = lessor
            .lease(TransmitPowerLevel::On.into_primitive())
            .await
            .map_err(|err| FatalTransmitSuspensionError::Fidl { context: "leasing", err })??;

        let mut required_level = HangingGetStream::new_with_fn_ptr(
            required_level,
            fpower_broker::RequiredLevelProxy::watch,
        );

        // Wait for the first required level.
        let current_level = next_required_level(&mut required_level).await?;
        update_current_level(&mut current_level_proxy, current_level).await?;

        Ok(Self {
            _lease: lease,
            _element_control: element_control,
            current_level_proxy,
            required_level,
            current_level,
            device,
        })
    }

    async fn wait(
        &mut self,
    ) -> Result<TransmitSuspensionRequest<'_>, FatalTransmitSuspensionError> {
        let Self {
            _lease: _,
            _element_control: _,
            current_level_proxy: _,
            required_level,
            current_level,
            device: _,
        } = self;

        loop {
            match current_level {
                TransmitPowerLevel::Off => return Ok(TransmitSuspensionRequest(self)),
                TransmitPowerLevel::On => {}
            }
            *current_level = next_required_level(required_level).await?;
        }
    }
}

pub(crate) struct TransmitSuspensionRequest<'a>(&'a mut EnabledTransmitSuspensionHandler);

impl TransmitSuspensionRequest<'_> {
    /// Handles a suspension request, blocking until the system resumes.
    ///
    /// Waits for the transmtting device to finish its work and then signals
    /// back to power framework that our power element's level is lowered. Then
    /// it waits for the level of our power element to rise before returning.
    /// The rise in power level indicates that it is okay for us to resume
    /// normal operation.
    pub(crate) async fn handle_suspension(self) -> Result<(), FatalTransmitSuspensionError> {
        let Self(EnabledTransmitSuspensionHandler {
            _lease: _,
            _element_control: _,
            current_level_proxy,
            required_level,
            current_level,
            device,
        }) = self;
        assert_eq!(current_level, &TransmitPowerLevel::Off);
        let name = device.bindings_id();

        // Wait for the device to be ready for suspension. Meanwhile, PF is
        // waiting for us to notify it that we've transitioned to the disabled
        // state.
        debug!("handling suspension for {name}. Waiting for tx done.");
        wait_device_tx_suspension_ready(device).await;
        debug!("suspension handling for {name} complete.");

        // Notify PF of our new level.
        //
        // This should allow the system to continue with suspension.
        update_current_level(current_level_proxy, TransmitPowerLevel::Off).await?;

        // Now we can wait until the system changes our required level back to
        // enabled.
        *current_level = loop {
            match next_required_level(required_level).await? {
                TransmitPowerLevel::Off => {}
                l @ TransmitPowerLevel::On => break l,
            }
        };
        update_current_level(current_level_proxy, *current_level).await?;
        debug!("resumed device {name} from suspension");

        Ok(())
    }
}

async fn wait_device_tx_suspension_ready(device: &WeakDeviceId<BindingsCtx>) {
    let Some(device) = device.upgrade() else {
        return;
    };
    match device.external_state() {
        DeviceSpecificInfo::Loopback(_) => {
            // No suspension handling for loopback.
        }
        DeviceSpecificInfo::Ethernet(EthernetInfo { netdevice, .. })
        | DeviceSpecificInfo::PureIp(PureIpDeviceInfo { netdevice, .. }) => {
            // For netdevice backed devices, wait for all tx descriptors to be
            // done before continuing with suspension.
            netdevice.handler.wait_tx_done().await;
        }
    };
}

async fn next_required_level(
    stream: &mut RequiredLevelStream,
) -> Result<TransmitPowerLevel, FatalTransmitSuspensionError> {
    let level = stream
        .by_ref()
        .next()
        .await
        .ok_or(FatalTransmitSuspensionError::RequiredLevelStreamEnded)?
        .map_err(|err| FatalTransmitSuspensionError::Fidl {
            context: "next required level",
            err,
        })??;
    TransmitPowerLevel::from_primitive(level)
        .ok_or_else(|| FatalTransmitSuspensionError::InvalidPowerLevel(level))
}

async fn update_current_level(
    proxy: &mut fpower_broker::CurrentLevelProxy,
    level: TransmitPowerLevel,
) -> Result<(), FatalTransmitSuspensionError> {
    Ok(proxy.update(level.into_primitive()).await.map_err(|err| {
        FatalTransmitSuspensionError::Fidl { context: "updating current level", err }
    })??)
}
