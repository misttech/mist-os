// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::shutdown_mocks::{new_mocks_provider, Admin, LeaseState, Signal};
use anyhow::Error;
use assert_matches::assert_matches;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
};
use futures::channel::mpsc;
use futures::{future, StreamExt};
use test_case::{test_case, test_matrix};
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_hardware_power_statecontrol as fstatecontrol,
    fidl_fuchsia_power as fpower, fidl_fuchsia_power_internal as fpower_internal,
    fidl_fuchsia_power_system as fsystem, fidl_fuchsia_sys2 as fsys,
    fidl_fuchsia_system_state as fdevicemanager, fuchsia_async as fasync,
};

mod shutdown_mocks;

const SHUTDOWN_SHIM_URL: &'static str =
    "fuchsia-pkg://fuchsia.com/shutdown-shim-integration-tests#meta/shutdown-shim.cm";

#[derive(PartialEq)]
enum RealmVariant {
    // Power manager is present, running, and actively handling requests.
    PowerManagerPresent,

    // Power manager hasn't started yet for whatever reason, and FIDL connections to it are hanging
    // as they wait for it to start.
    PowerManagerIsntStartedYet,

    // Power manager isn't present on the system, and FIDL connections to it are closed
    // immediately.
    PowerManagerNotPresent,
}

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
    variant: RealmVariant,
) -> Result<(RealmInstance, mpsc::UnboundedReceiver<Signal>), Error> {
    let (mocks_provider, recv_signals) = new_mocks_provider(is_power_framework_available);
    let builder = RealmBuilder::new().await?;

    let shutdown_shim =
        builder.add_child("shutdown-shim", SHUTDOWN_SHIM_URL, ChildOptions::new()).await?;
    let mocks_server =
        builder.add_local_child("mocks-server", mocks_provider, ChildOptions::new()).await?;

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
    // Give the shim the component_manager mock, as it is always available to the shim in prod
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fsys::SystemControllerMarker>())
                .from(&mocks_server)
                .to(&shutdown_shim),
        )
        .await?;

    match variant {
        RealmVariant::PowerManagerPresent => {
            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol::<fstatecontrol::AdminMarker>())
                        .from(&mocks_server)
                        .to(&shutdown_shim),
                )
                .await?;
        }
        RealmVariant::PowerManagerIsntStartedYet => {
            let black_hole = builder
                .add_local_child(
                    "black-hole",
                    move |handles: LocalComponentHandles| {
                        Box::pin(async move {
                            let _handles = handles;
                            // We want to hold the mock_handles for the lifetime of this mock component, but
                            // never do anything with them. This will cause FIDL requests to us to go
                            // unanswered, simulating the environment where a component is unable to launch due
                            // to pkgfs not coming online.
                            future::pending::<()>().await;
                            panic!("the black hole component should never return")
                        })
                    },
                    ChildOptions::new(),
                )
                .await?;
            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol::<fstatecontrol::AdminMarker>())
                        .from(&black_hole)
                        .to(&shutdown_shim),
                )
                .await?;
        }
        RealmVariant::PowerManagerNotPresent => (),
    }

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

#[test_matrix(
    [true, false],
    [RebootType::Reboot, RebootType::PerformReboot],
    [RebootReason::SystemUpdate, RebootReason::SessionFailure]
)]
#[fuchsia::test]
async fn power_manager_present_reboot(
    is_power_framework_available: bool,
    reboot_type: RebootType,
    reason: RebootReason,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    match reboot_type {
        RebootType::Reboot => shim_statecontrol.reboot(reason.as_reboot_reason()).await?.unwrap(),
        RebootType::PerformReboot => {
            shim_statecontrol.perform_reboot(&reason.as_reboot_options()).await?.unwrap()
        }
    }

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }

    let expected_admin_req = match reboot_type {
        RebootType::Reboot => Admin::Reboot(reason.as_reboot_reason()),
        RebootType::PerformReboot => Admin::PerformReboot(reason.as_reboot_options()),
    };
    let actual_admin_req = assert_matches!(
        recv_signals.next().await,
        Some(Signal::Statecontrol(req)) => req
    );
    assert_eq!(actual_admin_req, expected_admin_req);

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_reboot_to_bootloader(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    shim_statecontrol.reboot_to_bootloader().await?.unwrap();
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(
        recv_signals.next().await,
        Some(Signal::Statecontrol(Admin::RebootToBootloader))
    );
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_reboot_to_recovery(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    shim_statecontrol.reboot_to_recovery().await?.unwrap();
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Statecontrol(Admin::RebootToRecovery)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_poweroff(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    shim_statecontrol.poweroff().await?.unwrap();
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Statecontrol(Admin::Poweroff)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_mexec(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    let kernel_zbi = zx::Vmo::create(0).unwrap();
    let data_zbi = zx::Vmo::create(0).unwrap();
    shim_statecontrol.mexec(kernel_zbi, data_zbi).await?.unwrap();
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Statecontrol(Admin::Mexec)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_suspend_to_ram(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    shim_statecontrol.suspend_to_ram().await?.unwrap();
    let res = recv_signals.next().await;
    assert_matches!(res, Some(Signal::Statecontrol(Admin::SuspendToRam)));
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_present_collaborative_reboot(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerPresent).await?;
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
    let result = shim_initiator
        .perform_pending_reboot()
        .await
        .expect("perform pending reboot should succeed");
    assert_matches!(result.rebooting, Some(true));

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }

    let expected_reboot_options = fstatecontrol::RebootOptions {
        reasons: Some(vec![fstatecontrol::RebootReason2::SystemUpdate]),
        __source_breaking: fidl::marker::SourceBreaking,
    };
    let actual_reboot_options = assert_matches!(
        recv_signals.next().await,
        Some(Signal::Statecontrol(Admin::PerformReboot(options))) => options
    );
    assert_eq!(actual_reboot_options, expected_reboot_options);

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_missing_poweroff(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerIsntStartedYet).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    // We don't expect this task to ever complete, as the shutdown shim isn't actually killed (the
    // shutdown methods it calls are mocks after all).
    let _task = fasync::Task::spawn(async move {
        shim_statecontrol.poweroff().await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    });
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_matrix(
    [true, false],
    [RebootType::Reboot, RebootType::PerformReboot]
)]
#[fuchsia::test]
async fn power_manager_missing_reboot_system_update(
    is_power_framework_available: bool,
    reboot_type: RebootType,
) -> Result<(), Error> {
    let reason = RebootReason::SystemUpdate;
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerIsntStartedYet).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    // We don't expect this task to ever complete, as the shutdown shim isn't actually killed (the
    // shutdown methods it calls are mocks after all).
    let _task = fasync::Task::spawn(async move {
        match reboot_type {
            RebootType::Reboot => shim_statecontrol.reboot(reason.as_reboot_reason()).await,
            RebootType::PerformReboot => {
                shim_statecontrol.perform_reboot(&reason.as_reboot_options()).await
            }
        }
        .expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    });

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_missing_mexec(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerIsntStartedYet).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    // We don't expect this task to ever complete, as the shutdown shim isn't actually killed (the
    // shutdown methods it calls are mocks after all).
    let _task = fasync::Task::spawn(async move {
        let kernel_zbi = zx::Vmo::create(0).unwrap();
        let data_zbi = zx::Vmo::create(0).unwrap();
        shim_statecontrol.mexec(kernel_zbi, data_zbi).await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    });

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_not_present_poweroff(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerNotPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    fasync::Task::spawn(async move {
        shim_statecontrol.poweroff().await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    })
    .detach();

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_matrix(
    [true, false],
    [RebootType::Reboot, RebootType::PerformReboot],
    [RebootReason::SystemUpdate, RebootReason::Oom]
)]
#[fuchsia::test]
async fn power_manager_not_present_reboot(
    is_power_framework_available: bool,
    reboot_type: RebootType,
    reason: RebootReason,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerNotPresent).await?;
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

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_not_present_mexec(is_power_framework_available: bool) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerNotPresent).await?;
    let shim_statecontrol =
        realm_instance.root.connect_to_protocol_at_exposed_dir::<fstatecontrol::AdminMarker>()?;

    fasync::Task::spawn(async move {
        let kernel_zbi = zx::Vmo::create(0).unwrap();
        let data_zbi = zx::Vmo::create(0).unwrap();
        shim_statecontrol.mexec(kernel_zbi, data_zbi).await.expect_err(
            "the shutdown shim should close the channel when manual shutdown driving is complete",
        );
    })
    .detach();

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }
    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));
    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
}

#[test_case(true; "with_power_framework")]
#[test_case(false; "without_power_framework")]
#[fuchsia::test]
async fn power_manager_not_present_collaborative_reboot(
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let (realm_instance, mut recv_signals) =
        new_realm(is_power_framework_available, RealmVariant::PowerManagerNotPresent).await?;
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

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Acquired))
        );
    }

    assert_matches!(recv_signals.next().await, Some(Signal::Sys2Shutdown(_)));

    if is_power_framework_available {
        assert_matches!(
            recv_signals.next().await,
            Some(Signal::ShutdownControlLease(LeaseState::Dropped))
        );
    }
    Ok(())
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
