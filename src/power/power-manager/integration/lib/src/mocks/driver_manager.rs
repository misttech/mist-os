// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::mocks::temperature_driver::MockTemperatureDriver,
    fidl_fuchsia_device_manager as fdevmgr, fidl_fuchsia_hardware_power_statecontrol as fpower,
    fidl_fuchsia_io as fio, fidl_fuchsia_power_manager as fpowermanager, fuchsia_async as fasync,
    fuchsia_component_test::LocalComponentHandles,
    futures::{FutureExt as _, TryStreamExt as _},
    parking_lot::RwLock,
    std::sync::Arc,
    tracing::*,
    vfs::directory::{
        entry::DirectoryEntry as _, helper::DirectlyMutable as _,
        immutable::simple::Simple as SimpleMutableDir,
    },
};

/// Mocks the Driver Manager to be used in integration tests.
pub struct MockDriverManager {
    devfs: Arc<vfs::directory::immutable::Simple>,
    current_termination_state: Arc<RwLock<Option<fpower::SystemPowerState>>>,
}

impl MockDriverManager {
    pub fn new() -> Arc<MockDriverManager> {
        Arc::new(Self {
            devfs: vfs::directory::immutable::simple::simple(),
            current_termination_state: Arc::new(RwLock::new(None)),
        })
    }

    /// Adds a MockTemperatureDriver to the devfs maintained by this mock.
    pub fn add_temperature_mock(&self, path: &str, mock: Arc<MockTemperatureDriver>) {
        let mut root = self.devfs.clone();
        let path = path.strip_prefix("/dev/").expect("Driver paths should start with /dev/");

        let (parent_path, device_name) = {
            let path = std::path::Path::new(&path);
            let file_name = path
                .file_name()
                .expect("path does not end in a normal file or directory name")
                .to_str()
                .expect("invalid file name");
            let parent = path.parent().expect("path terminates in a root");
            info!("parent={:?}, file_name={:?}", parent, file_name);
            (parent, file_name)
        };

        for component in parent_path.components() {
            let component =
                component.as_os_str().to_str().expect("invalid path component").to_string();
            root = root
                .get_or_insert(component, vfs::directory::immutable::simple::simple)
                .into_any()
                .downcast::<SimpleMutableDir>()
                .unwrap();
        }

        root.add_entry(device_name, mock.vfs_service()).unwrap();
    }

    /// Runs the mock using the provided `LocalComponentHandles`.
    ///
    /// Expected usage is to call this function from a closure for the
    /// `local_component_implementation` parameter to `RealmBuilder.add_local_child`.
    ///
    /// For example:
    ///     let mock_driver_manager = MockDriverManager::new();
    ///     let driver_manager_child = realm_builder
    ///         .add_local_child(
    ///             "driver_manager",
    ///             move |handles| Box::pin(mock_driver_manager.clone().run(handles)),
    ///             ChildOptions::new().eager(),
    ///         )
    ///         .await
    ///         .unwrap();
    ///
    pub async fn run(self: Arc<Self>, handles: LocalComponentHandles) -> Result<(), anyhow::Error> {
        info!("MockDriverManager: run");
        let registration_proxy = handles
            .connect_to_protocol::<fpowermanager::DriverManagerRegistrationMarker>()
            .unwrap();

        let scope = vfs::execution_scope::ExecutionScope::new();
        let (client, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        let () = self.devfs.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            fio::MODE_TYPE_DIRECTORY,
            vfs::path::Path::dot(),
            server,
        );

        let mut fs = fuchsia_component::server::ServiceFs::new();
        let () = fs.add_remote_node("dev", client);
        let fs = fs.serve_connection(handles.outgoing_dir).unwrap();

        futures::future::try_join3(
            self.run_inner(registration_proxy),
            scope.wait().map(Ok),
            fs.try_collect(),
        )
        .await
        .map(|((), (), ())| ())
    }

    async fn run_inner(
        self: Arc<Self>,
        registration_proxy: fpowermanager::DriverManagerRegistrationProxy,
    ) -> Result<(), anyhow::Error> {
        self.register_with_power_manager(registration_proxy).await;
        Ok(())
    }

    async fn register_with_power_manager(
        &self,
        registration_proxy: fpowermanager::DriverManagerRegistrationProxy,
    ) {
        info!("MockDriverManager: registering with Power Manager");

        let (transition_client, mut transition_request_stream) =
            fidl::endpoints::create_request_stream::<fdevmgr::SystemStateTransitionMarker>()
                .unwrap();

        registration_proxy
            .register(transition_client)
            .await
            .expect("FIDL error: Failed to register power manager")
            .expect("Registration error: Failed to register power manager");

        let current_state = self.current_termination_state.clone();
        fasync::Task::local(async move {
            while let Some(fdevmgr::SystemStateTransitionRequest::SetTerminationSystemState {
                state,
                responder,
            }) = transition_request_stream.try_next().await.unwrap()
            {
                info!("MockDriverManager: received SetTerminationSystemState request: {:?}", state);
                *current_state.write() = Some(state);
                let _ = responder.send(&mut Ok(()));
            }
        })
        .detach();
    }

    pub fn get_current_termination_state(&self) -> fpower::SystemPowerState {
        self.current_termination_state.read().expect("Termination state not set")
    }
}

#[cfg(test)]
mod tests {
    use {super::*, assert_matches::assert_matches, futures::StreamExt as _};

    /// Tests that the mock tries to register with Power Manager when it first starts up.
    #[fuchsia::test]
    async fn test_power_manager_registration() {
        // Create and serve the mock service
        let (registration_proxy, mut registration_stream) =
            fidl::endpoints::create_proxy_and_stream::<
                fpowermanager::DriverManagerRegistrationMarker,
            >()
            .unwrap();
        let mock = MockDriverManager::new();
        let _task = fasync::Task::local(mock.clone().run_inner(registration_proxy));

        assert_matches!(
            registration_stream.next().await.unwrap().unwrap(),
            fpowermanager::DriverManagerRegistrationRequest::Register { .. }
        );
    }

    /// Tests that system state transition requests sent to the mock are reflected in calls to
    /// `get_current_termination_state`.
    #[fuchsia::test]
    async fn test_transition_state() {
        // Create and serve the mock service
        let (registration_proxy, mut registration_stream) =
            fidl::endpoints::create_proxy_and_stream::<
                fpowermanager::DriverManagerRegistrationMarker,
            >()
            .unwrap();
        let mock = MockDriverManager::new();
        let _task = fasync::Task::local(mock.clone().run_inner(registration_proxy));

        let fpowermanager::DriverManagerRegistrationRequest::Register {
            system_state_transition,
            responder,
            ..
        } = registration_stream.next().await.unwrap().unwrap();
        assert_matches!(responder.send(&mut Ok(())), Ok(()));
        let system_state_transition = system_state_transition.into_proxy().unwrap();

        system_state_transition
            .set_termination_system_state(fpower::SystemPowerState::RebootRecovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mock.get_current_termination_state(), fpower::SystemPowerState::RebootRecovery);

        system_state_transition
            .set_termination_system_state(fpower::SystemPowerState::SuspendRam)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mock.get_current_termination_state(), fpower::SystemPowerState::SuspendRam);
    }
}
