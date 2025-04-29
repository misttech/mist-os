// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::shutdown_mocks::{new_mocks_provider, LeaseState, Signal};
use anyhow::Error;
use assert_matches::assert_matches;
use fidl::marker::SourceBreaking;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::channel::mpsc;
use futures::StreamExt;
use test_case::{test_case, test_matrix};
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_hardware_power_statecontrol as fstatecontrol,
    fidl_fuchsia_power as fpower, fidl_fuchsia_power_internal as fpower_internal,
    fidl_fuchsia_power_system as fsystem, fidl_fuchsia_sys2 as fsys,
    fidl_fuchsia_system_state as fdevicemanager, fuchsia_async as fasync,
};

use crate::reboot_watcher_client::{DeprecatedRebootWatcherClient, RebootWatcherClient};

mod reboot_watcher_client;
mod shutdown_mocks;

const SHUTDOWN_SHIM_URL: &'static str =
    "fuchsia-pkg://fuchsia.com/shutdown-shim-integration-tests#meta/shutdown-shim.cm";

// Makes a new realm containing a shutdown shim and a mocks server.
//
//              root
//             /    \
// shutdown-shim    mocks-server
//
// The mocks server is seeded with an mpsc::UnboundedSender, and whenever the shutdown-shim calls a
// function on the mocks server the mocks server will emit a shutdown_mocks::Signal over the
// channel.
//
// The shutdown-shim always receives logging from above the root, along with mock component_manager
// protocols from the mocks-server (because these are always present in prod). The `variant` field
// determines whether the shim receives a functional version of the power_manager mocks.
async fn new_realm(
    is_power_framework_available: bool,
) -> Result<(RealmInstance, mpsc::UnboundedReceiver<Signal>), Error> {
    let (mocks_provider, recv_signals) = new_mocks_provider(is_power_framework_available);
    let builder = RealmBuilder::new().await?;

    let shutdown_shim =
        builder.add_child("shutdown-shim", SHUTDOWN_SHIM_URL, ChildOptions::new()).await?;
    let mocks_server =
        builder.add_local_child("mocks-server", mocks_provider, ChildOptions::new()).await?;

    // Give config capabilities.
    if is_power_framework_available {
        builder
            .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                name: "fuchsia.power.SuspendEnabled".parse()?,
                value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(true)),
            }))
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.power.SuspendEnabled"))
                    .from(Ref::self_())
                    .to(&shutdown_shim),
            )
            .await?;
    } else {
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.power.SuspendEnabled"))
                    .from(Ref::void())
                    .to(&shutdown_shim),
            )
            .await?;
    }

    // Give the shim logging
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fboot::WriteOnlyLogMarker>())
                .from(Ref::parent())
                .to(&shutdown_shim),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&mocks_server),
        )
        .await?;
    // Expose the shim's statecontrol.Admin so test cases can access it
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fstatecontrol::AdminMarker>())
                .from(&shutdown_shim)
                .to(Ref::parent()),
        )
        .await?;
    // Expose the shim's Collaborative Reboot protocols so test cases can access
    // them.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<
                    fpower_internal::CollaborativeRebootSchedulerMarker,
                >())
                .from(&shutdown_shim)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fpower::CollaborativeRebootInitiatorMarker>())
                .from(&shutdown_shim)
                .to(Ref::parent()),
        )
        .await?;
    // Expose the shim's statecontrol.RebootMethodsWatcherRegister so test cases can access it
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<fstatecontrol::RebootMethodsWatcherRegisterMarker>(),
                )
                .from(&shutdown_shim)
                .to(Ref::parent()),
        )
        .await?;
    // Give the shim the component_manager mock, as it is always available to the shim in prod
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fsys::SystemControllerMarker>())
                .from(&mocks_server)
                .to(&shutdown_shim),
        )
        .await?;

    if is_power_framework_available {
        // Give the shim access to mock activity governor.
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fsystem::ActivityGovernorMarker>())
                    .from(&mocks_server)
                    .to(&shutdown_shim),
            )
            .await?;
    }

    Ok((builder.build().await?, recv_signals))
}

enum RebootType {
    // Corresponds to the deprecated `Admin.Reboot` method.
    Reboot,
    // Corresponds to the `Admin.PerformReboot` method.
    PerformReboot,
}

enum RebootReason {
    SystemUpdate,
    SessionFailure,
    Oom,
}

impl RebootReason {
    fn as_reboot_reason(&self) -> fstatecontrol::RebootReason {
        match self {
            Self::SystemUpdate => fstatecontrol::RebootReason::SystemUpdate,
            Self::SessionFailure => fstatecontrol::RebootReason::SessionFailure,
            Self::Oom => fstatecontrol::RebootReason::OutOfMemory,
        }
    }

    fn as_reboot_options(&self) -> fstatecontrol::RebootOptions {
        let reason = match self {
            Self::SystemUpdate => fstatecontrol::RebootReason2::SystemUpdate,
            Self::SessionFailure => fstatecontrol::RebootReason2::SessionFailure,
            Self::Oom => fstatecontrol::RebootReason2::OutOfMemory,
        };
        fstatecontrol::RebootOptions {
            reasons: Some(vec![reason]),
            __source_breaking: fidl::marker::SourceBreaking,
        }
    }
}

async fn verify_shutdown_shim_common_behavior(
    realm_instance: RealmInstance,
    is_power_framework_available: bool,
    mut recv_signals: mpsc::UnboundedReceiver<Signal>,
) -> Result<(), Error> {
    // Conditionally checks the mock receives the TakeWakeLease call
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }

    // Verify the system controller service gets the shutdown request from shutdown-shim
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));

    // Conditionally checks the mock detects the lease drop.
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    } else {
        drop(realm_instance);
        assert_matches!(recv_signals.next().await, None);
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn test_poweroff(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    let _task = fasync::Task::spawn(async move {
        shim_statecontrol.poweroff().await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    });
    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn test_mexec(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    let _task = fasync::Task::spawn(async move {
        let kernel_zbi = zx::Vmo::create(0).unwrap();
        let data_zbi = zx::Vmo::create(0).unwrap();
        shim_statecontrol.mexec(kernel_zbi, data_zbi).await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    });

    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

#[test_matrix(
    [true, false],
    [RebootType::Reboot, RebootType::PerformReboot],
    [RebootReason::SystemUpdate, RebootReason::Oom, RebootReason::SessionFailure]
)]
#[fuchsia::test]
async fn test_reboot(
    is_power_framework_available: bool,
    reboot_type: RebootType,
    reason: RebootReason,
) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    fasync::Task::spawn(async move {
        match reboot_type {
            RebootType::Reboot => shim_statecontrol.reboot(reason.as_reboot_reason()).await,
            RebootType::PerformReboot => {
                shim_statecontrol.perform_reboot(&reason.as_reboot_options()).await
            }
        }
        .expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    })
    .detach();

    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn test_collaborative_reboot(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;
    let shim_scheduler = realm_instance
        .root
        .connect_to_protocol_at_exposed_dir::<fpower_internal::CollaborativeRebootSchedulerMarker>(
        )?;
    let shim_initiator = realm_instance
        .root
        .connect_to_protocol_at_exposed_dir::<fpower::CollaborativeRebootInitiatorMarker>()?;

    shim_scheduler
        .schedule_reboot(fpower_internal::CollaborativeRebootReason::SystemUpdate, None)
        .await
        .expect("failed to schedule reboot");
    fasync::Task::spawn(async move {
        shim_initiator.perform_pending_reboot().await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    })
    .detach();

    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

/// Verifies that `fuchsia.hardware.power.statecontrol/Admin.Reboot` requests
/// sent to shutdown-shim are handled as expected:
///     1) forward the reboot reason to connected RebootWatcher clients
///     2) send a shutdown request to the system controller
// TODO(https://fxbug.dev/385742868): Delete this test once the API is removed.
#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn deprecated_shutdown_test(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;

    // Verify both the "current" and "deprecated" watcher APIs.
    let mut reboot_watcher = RebootWatcherClient::new(&realm_instance).await;
    let mut deprecated_reboot_watcher = DeprecatedRebootWatcherClient::new(&realm_instance).await;

    // Send a reboot request to shutdown-shim (in a separate Task because it never returns)
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;
    let _shutdown_task =
        fasync::Task::local(shim_statecontrol.reboot(fstatecontrol::RebootReason::SystemUpdate));

    // Verify shutdown-shim forwards the reboot reason to the watcher client
    let reasons = assert_matches!(
        reboot_watcher.get_reboot_options().await,
        fstatecontrol::RebootOptions{ reasons: Some(reasons), ..} => reasons
    );
    assert_eq!(&reasons[..], [fstatecontrol::RebootReason2::SystemUpdate]);
    assert_eq!(
        deprecated_reboot_watcher.get_reboot_reason().await,
        fstatecontrol::RebootReason::SystemUpdate
    );

    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

/// Verifies that `fuchsia.hardware.power.statecontrol/Admin.PerformReboot`
/// requests sent to shutdown-shim are handled as expected:
///     1) forward the reboot reason to connected RebootWatcher clients
///     2) send a shutdown request to the system controller
#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn shutdown_test(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, recv_signals) = new_realm(is_power_framework_available).await?;

    // Verify both the "current" and "deprecated" watcher APIs.
    let mut reboot_watcher = RebootWatcherClient::new(&realm_instance).await;
    let mut deprecated_reboot_watcher = DeprecatedRebootWatcherClient::new(&realm_instance).await;

    // Send a reboot request to the shutdown-shim (in a separate Task because it never returns)
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;
    let options = fstatecontrol::RebootOptions {
        reasons: Some(vec![
            fstatecontrol::RebootReason2::SystemUpdate,
            fstatecontrol::RebootReason2::NetstackMigration,
        ]),
        __source_breaking: SourceBreaking,
    };
    let _shutdown_task = fasync::Task::local(shim_statecontrol.perform_reboot(&options));

    // Verify shutdown-shim forwards the reboot reason to the watcher client
    let reasons = assert_matches!(
        reboot_watcher.get_reboot_options().await,
        fstatecontrol::RebootOptions{ reasons: Some(reasons), ..} => reasons
    );
    assert_eq!(
        &reasons[..],
        [
            fstatecontrol::RebootReason2::SystemUpdate,
            fstatecontrol::RebootReason2::NetstackMigration
        ]
    );
    // TODO(https://fxbug.dev/385742868): Delete the deprecated watcher assertions once the API
    // is removed. For now, verify that a backwards-compatible reason is provided.
    assert_eq!(
        deprecated_reboot_watcher.get_reboot_reason().await,
        fstatecontrol::RebootReason::SystemUpdate
    );

    verify_shutdown_shim_common_behavior(realm_instance, is_power_framework_available, recv_signals)
        .await
}

#[fuchsia::test]
// If this test fails it is because a new variant was introduced. If a new
// variant is introduced the `send_command` function in shutdown-shim must be
// updated to support it.
async fn test_variant_coverage() -> Result<(), Error> {
    #[allow(dead_code)] // TODO(https://fxbug.dev/351850765)
    struct Mock {}

    impl std::convert::From<fdevicemanager::SystemPowerState> for Mock {
        fn from(ps: fdevicemanager::SystemPowerState) -> Mock {
            match ps {
                fdevicemanager::SystemPowerState::Reboot
                | fdevicemanager::SystemPowerState::FullyOn
                | fdevicemanager::SystemPowerState::RebootBootloader
                | fdevicemanager::SystemPowerState::RebootRecovery
                | fdevicemanager::SystemPowerState::Poweroff
                | fdevicemanager::SystemPowerState::Mexec
                | fdevicemanager::SystemPowerState::SuspendRam
                | fdevicemanager::SystemPowerState::RebootKernelInitiated => Mock {},
            }
        }
    }
    Ok(())
}
