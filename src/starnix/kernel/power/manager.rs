// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    power::{SuspendState, SuspendStats},
    task::CurrentTask,
};

use std::{collections::HashSet, sync::Arc};

use anyhow::{anyhow, Context};
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_kernel as fkernel;
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_sync};
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::StreamExt;
use once_cell::sync::{Lazy, OnceCell};
use starnix_logging::log_error;
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::{error, errors::Errno, from_status_like_fdio};

static CPU_RESOURCE: Lazy<zx::Resource> = Lazy::new(|| {
    connect_to_protocol_sync::<fkernel::CpuResourceMarker>()
        .expect("couldn't connect to fuchsia.kernel.CpuResource")
        .get(zx::Time::INFINITE)
        .expect("couldn't talk to fuchsia.kernel.CpuResource")
});

/// Manager for suspend and resume.
///
#[derive(Default)]
pub struct SuspendResumeManager {
    /// Synch FIDL Proxy to create leases on the power topology.
    ///
    /// Power Mode power element is owned and registered by Starnix kernel. This power element is
    /// added in the power topology as a dependent on Application Activity element that is owned by
    /// the SAG.
    ///
    /// After Starnix boots, a power-on lease will be created and retained.
    ///
    /// When needs to suspend, Starnix should create another lease for the suspend state and release
    /// the power-on lease.
    ///
    /// The power level will only be changed to the requested level when all elements in the
    /// topology can maintain the minimum power equilibrium in the lease.
    ///
    /// | Power Mode        | Level |
    /// | ----------------- | ----- |
    /// | On                | 4     |
    /// | Susepnd-to-Idle   | 3     |
    /// | Standby           | 2     |
    /// | Susepnd-to-RAM    | 1     |
    /// | Susepnd-to-Disk   | 0     |
    power_mode_lessor: OnceCell<fbroker::LessorSynchronousProxy>,
    inner: Mutex<SuspendResumeManagerInner>,
}
static POWER_ON_LEVEL: fbroker::PowerLevel = 4;

/// Manager for suspend and resume.
#[derive(Default)]
pub struct SuspendResumeManagerInner {
    suspend_stats: SuspendStats,
    sync_on_suspend_enabled: bool,
    /// Lease control to hold the system power state as active.
    power_on_control: Option<fbroker::LeaseControlSynchronousProxy>,
}

pub type SuspendResumeManagerHandle = Arc<SuspendResumeManager>;

impl SuspendResumeManager {
    /// Locks and returns the inner state of the manager.
    fn lock(&self) -> MutexGuard<'_, SuspendResumeManagerInner> {
        self.inner.lock()
    }

    /// Power on the PowerMode element and start listening to the suspend stats updates.
    pub fn init(
        self: &SuspendResumeManagerHandle,
        system_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        let topology = connect_to_protocol_sync::<fbroker::TopologyMarker>()?;
        let activity_governor = connect_to_protocol_sync::<fsystem::ActivityGovernorMarker>()?;

        // Create the PowerMode power element depending on the Execution State of SAG.
        let power_elements = activity_governor
            .get_power_elements(zx::Time::INFINITE)
            .context("cannot get Activity Governor element from SAG")?;
        if let Some(Some(application_activity_token)) = power_elements
            .application_activity
            .map(|application_activity| application_activity.active_dependency_token)
        {
            // TODO(b/316023943): also depends on execution_resume_latency after implemented.
            let power_levels: Vec<u8> = (0..=POWER_ON_LEVEL).collect();
            let (_, lessor_client_end, _) = topology
                .add_element(
                    "starnix_power_mode",
                    POWER_ON_LEVEL,
                    power_levels.as_slice(),
                    vec![fbroker::LevelDependency {
                        dependency_type: fbroker::DependencyType::Active,
                        dependent_level: POWER_ON_LEVEL,
                        requires_token: application_activity_token,
                        requires_level: fsystem::APPLICATION_ACTIVITY_ACTIVE,
                    }],
                    vec![],
                    vec![],
                    zx::Time::INFINITE,
                )?
                .map_err(|e| anyhow!("PowerBroker::AddElementError({e:?})"))?;

            // Power on by holding a lease.
            let lessor = lessor_client_end.into_sync_proxy();
            let power_on_control = lessor
                .lease(POWER_ON_LEVEL, zx::Time::INFINITE)?
                .map_err(|e| anyhow!("PowerBroker::LeaseError({e:?})"))?
                .into_sync_proxy();

            self.power_mode_lessor.set(lessor).expect("Power Mode should be uninitialized");
            self.lock().power_on_control = Some(power_on_control);
        };

        let self_ref = self.clone();
        system_task.kernel().kthreads.spawn_future(async move {
            // Start listening to the suspend stats updates
            let stats_proxy = connect_to_protocol::<fsuspend::StatsMarker>()
                .expect("connection to fuchsia.power.suspend.Stats");
            let mut stats_stream = HangingGetStream::new(stats_proxy, fsuspend::StatsProxy::watch);
            while let Some(stream) = stats_stream.next().await {
                match stream {
                    Ok(stats) => {
                        let stats_guard = &mut self_ref.lock().suspend_stats;
                        stats_guard.success_count = stats.success_count.unwrap_or_default();
                        stats_guard.fail_count = stats.fail_count.unwrap_or_default();
                        stats_guard.last_time_in_sleep = zx::Duration::from_millis(
                            stats.last_time_in_suspend.unwrap_or_default(),
                        );
                        stats_guard.last_time_in_suspend_operations = zx::Duration::from_millis(
                            stats.last_time_in_suspend_operations.unwrap_or_default(),
                        );
                    }
                    Err(e) => {
                        log_error!("stats watcher got an error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub fn suspend_stats(&self) -> SuspendStats {
        self.lock().suspend_stats.clone()
    }

    pub fn sync_on_suspend_enabled(&self) -> bool {
        self.lock().sync_on_suspend_enabled.clone()
    }

    pub fn set_sync_on_suspend(&self, enable: bool) {
        self.lock().sync_on_suspend_enabled = enable;
    }

    pub fn suspend_states(&self) -> HashSet<SuspendState> {
        // TODO(b/326470421): Remove the hardcoded supported state.
        HashSet::from([SuspendState::Ram, SuspendState::Idle])
    }

    fn update_power_lease(
        &self,
        level: fbroker::PowerLevel,
    ) -> Result<fbroker::LeaseControlSynchronousProxy, Errno> {
        if let Some(lessor) = self.power_mode_lessor.get() {
            match lessor.lease(level, zx::Time::INFINITE) {
                Ok(Ok(res)) => Ok(res.into_sync_proxy()),
                Ok(Err(err)) => {
                    log_error!("power broker lease error {:?}", err);
                    error!(EINVAL)
                }
                Err(err) => {
                    log_error!("power broker fidl error {:?}", err);
                    error!(EINVAL)
                }
            }
        } else {
            log_error!("power-mode element is not initialized");
            error!(EAGAIN)
        }
    }

    pub fn suspend(&self, state: SuspendState) -> Result<(), Errno> {
        // Before the old lease is dropped, a new lease must be created to transit to the new level.
        // This ensures a smooth transition without going back to the initial power level.
        let lease_control = self.update_power_lease(state.into())?;
        {
            // Clear the lease control and drop it.
            self.lock().power_on_control = None;
        }

        // TODO(b/316023943): Execute ops of suspend state transition via SAG suspend fidl api.
        // Temporary hack to trigger system suspend directly.
        let resume_at = zx::Time::after(zx::Duration::from_seconds(5));
        zx::Status::ok(unsafe {
            zx::sys::zx_system_suspend_enter(CPU_RESOURCE.raw_handle(), resume_at.into_nanos())
        })
        .map_err(|status| from_status_like_fdio!(status))?;

        // TODO(b/322789559): power on when wake signal pathway is ready.
        self.lock().power_on_control = Some(self.update_power_lease(POWER_ON_LEVEL)?);
        drop(lease_control);

        Ok(())
    }
}
