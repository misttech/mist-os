// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod client_connectors;
mod mocks;

use {
    crate::mocks::{
        activity_service::MockActivityService, input_settings_service::MockInputSettingsService,
        system_controller::MockSystemControllerService,
    },
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl::AsHandleRef as _,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_hardware_power_statecontrol as fpower,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_powermanager_driver_temperaturecontrol as ftemperaturecontrol,
    fidl_fuchsia_testing as ftesting,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    std::sync::atomic::{AtomicU64, Ordering},
    std::sync::Arc,
    tracing::*,
};

const POWER_MANAGER_URL: &str = "#meta/power-manager.cm";
const MOCK_COBALT_URL: &str = "#meta/mock_cobalt.cm";
const FAKE_CLOCK_URL: &str = "#meta/fake_clock.cm";

/// Increase the time scale so Power Manager's interval-based operation runs faster for testing.
const FAKE_TIME_SCALE: u32 = 100;

/// Unique number that is incremented for each TestEnv to avoid name clashes.
static UNIQUE_REALM_NUMBER: AtomicU64 = AtomicU64::new(0);

pub struct TestEnvBuilder {
    power_manager_node_config_path: Option<String>,
}

impl TestEnvBuilder {
    pub fn new() -> Self {
        Self { power_manager_node_config_path: None }
    }

    /// Sets the node config path that Power Manager will be configured with.
    pub fn power_manager_node_config_path(mut self, path: &str) -> Self {
        self.power_manager_node_config_path = Some(path.into());
        self
    }

    pub async fn build(self) -> TestEnv {
        // Generate a unique realm name based on the current process ID and unique realm number for
        // the current process.
        let realm_name = format!(
            "{}-{}",
            fuchsia_runtime::process_self().get_koid().unwrap().raw_koid(),
            UNIQUE_REALM_NUMBER.fetch_add(1, Ordering::Relaxed)
        );

        let realm_builder =
            RealmBuilder::with_params(RealmBuilderParams::new().realm_name(realm_name))
                .await
                .expect("Failed to create RealmBuilder");

        realm_builder.driver_test_realm_setup().await.expect("Failed to setup driver test realm");

        let power_manager = realm_builder
            .add_child("power_manager", POWER_MANAGER_URL, ChildOptions::new())
            .await
            .expect("Failed to add child: power_manager");

        let mock_cobalt = realm_builder
            .add_child("mock_cobalt", MOCK_COBALT_URL, ChildOptions::new())
            .await
            .expect("Failed to add child: mock_cobalt");

        let fake_clock = realm_builder
            .add_child("fake_clock", FAKE_CLOCK_URL, ChildOptions::new())
            .await
            .expect("Failed to add child: fake_clock");

        let activity_service = MockActivityService::new();
        let activity_service_clone = activity_service.clone();
        let activity_service_child = realm_builder
            .add_local_child(
                "activity_service",
                move |handles| Box::pin(activity_service_clone.clone().run(handles)),
                ChildOptions::new(),
            )
            .await
            .expect("Failed to add child: activity_service");

        let input_settings_service = MockInputSettingsService::new();
        let input_settings_service_clone = input_settings_service.clone();
        let input_settings_service_child = realm_builder
            .add_local_child(
                "input_settings_service",
                move |handles| Box::pin(input_settings_service_clone.clone().run(handles)),
                ChildOptions::new(),
            )
            .await
            .expect("Failed to add child: input_settings_service");

        let system_controller_service = MockSystemControllerService::new();
        let system_controller_service_clone = system_controller_service.clone();
        let system_controller_service_child = realm_builder
            .add_local_child(
                "system_controller_service",
                move |handles| Box::pin(system_controller_service_clone.clone().run(handles)),
                ChildOptions::new(),
            )
            .await
            .expect("Failed to add child: system_controller_service");

        // Set up Power Manager's required routes
        let parent_to_power_manager_routes = Route::new()
            .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
            .capability(Capability::protocol_by_name("fuchsia.tracing.provider.Registry"))
            .capability(Capability::protocol_by_name("fuchsia.boot.WriteOnlyLog"));
        realm_builder
            .add_route(parent_to_power_manager_routes.from(Ref::parent()).to(&power_manager))
            .await
            .unwrap();

        let parent_to_cobalt_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.logger.LogSink"));
        realm_builder
            .add_route(parent_to_cobalt_routes.from(Ref::parent()).to(&mock_cobalt))
            .await
            .unwrap();

        let parent_to_fake_clock_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.logger.LogSink"));
        realm_builder
            .add_route(parent_to_fake_clock_routes.from(Ref::parent()).to(&fake_clock))
            .await
            .unwrap();

        let fake_clock_to_power_manager_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.testing.FakeClock"));
        realm_builder
            .add_route(fake_clock_to_power_manager_routes.from(&fake_clock).to(&power_manager))
            .await
            .unwrap();

        let fake_clock_to_parent_routes = Route::new()
            .capability(Capability::protocol_by_name("fuchsia.testing.FakeClockControl"));
        realm_builder
            .add_route(fake_clock_to_parent_routes.from(&fake_clock).to(Ref::parent()))
            .await
            .unwrap();

        let cobalt_to_power_manager_routes = Route::new()
            .capability(Capability::protocol_by_name("fuchsia.metrics.MetricEventLoggerFactory"));
        realm_builder
            .add_route(cobalt_to_power_manager_routes.from(&mock_cobalt).to(&power_manager))
            .await
            .unwrap();

        let activity_service_to_power_manager_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.ui.activity.Provider"));
        realm_builder
            .add_route(
                activity_service_to_power_manager_routes
                    .from(&activity_service_child)
                    .to(&power_manager),
            )
            .await
            .unwrap();

        let input_settings_service_to_power_manager_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.settings.Input"));
        realm_builder
            .add_route(
                input_settings_service_to_power_manager_routes
                    .from(&input_settings_service_child)
                    .to(&power_manager),
            )
            .await
            .unwrap();

        let system_controller_to_power_manager_routes =
            Route::new().capability(Capability::protocol_by_name("fuchsia.sys2.SystemController"));
        realm_builder
            .add_route(
                system_controller_to_power_manager_routes
                    .from(&system_controller_service_child)
                    .to(&power_manager),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("pkg")
                            .subdir("config/power_manager")
                            .as_("config")
                            .path("/config")
                            .rights(fio::R_STAR_DIR),
                    )
                    .from(Ref::framework())
                    .to(&power_manager),
            )
            .await
            .unwrap();

        let power_manager_to_parent_routes = Route::new()
            .capability(Capability::protocol_by_name(
                "fuchsia.hardware.power.statecontrol.RebootMethodsWatcherRegister",
            ))
            .capability(Capability::protocol_by_name("fuchsia.power.profile.Watcher"))
            .capability(Capability::protocol_by_name("fuchsia.thermal.ClientStateConnector"))
            .capability(Capability::protocol_by_name("fuchsia.power.clientlevel.Connector"))
            .capability(Capability::protocol_by_name("fuchsia.hardware.power.statecontrol.Admin"));
        realm_builder
            .add_route(power_manager_to_parent_routes.from(&power_manager).to(Ref::parent()))
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::directory("dev-topological"))
                    .from(Ref::child("driver_test_realm"))
                    .to(&power_manager),
            )
            .await
            .unwrap();

        // Update Power Manager's structured config values
        realm_builder.init_mutable_config_from_package(&power_manager).await.unwrap();
        realm_builder
            .set_config_value_string(
                &power_manager,
                "node_config_path",
                self.power_manager_node_config_path
                    .as_ref()
                    .expect("power_manager_node_config_path not set"),
            )
            .await
            .unwrap();

        // Finally, build it
        let realm_instance = realm_builder.build().await.expect("Failed to build RealmInstance");

        // Start driver test realm
        let args = fdt::RealmArgs {
            root_driver: Some("#meta/root.cm".to_string()),
            use_driver_framework_v2: Some(true),
            ..Default::default()
        };

        realm_instance
            .driver_test_realm_start(args)
            .await
            .expect("Failed to start driver test realm");

        // Increase the time scale so Power Manager's interval-based operation runs faster for
        // testing
        set_fake_time_scale(&realm_instance, FAKE_TIME_SCALE).await;

        TestEnv {
            realm_instance: Some(realm_instance),
            mocks: Mocks { activity_service, input_settings_service, system_controller_service },
        }
    }
}

pub struct TestEnv {
    realm_instance: Option<RealmInstance>,
    pub mocks: Mocks,
}

impl TestEnv {
    /// Connects to a protocol exposed by a component within the RealmInstance.
    pub fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> P::Proxy {
        self.realm_instance
            .as_ref()
            .unwrap()
            .root
            .connect_to_protocol_at_exposed_dir::<P>()
            .unwrap()
    }

    /// Destroys the TestEnv and underlying RealmInstance.
    ///
    /// Every test that uses TestEnv must call this at the end of the test.
    pub async fn destroy(&mut self) {
        info!("Destroying TestEnv");
        self.realm_instance
            .take()
            .expect("Missing realm instance")
            .destroy()
            .await
            .expect("Failed to destroy realm instance");
    }

    /// Sets the temperature for a mock temperature device.
    pub async fn set_temperature(&self, driver_path: &str, temperature: f32) {
        let dev = self.realm_instance.as_ref().unwrap().driver_test_realm_connect_to_dev().unwrap();

        let control_path = driver_path.strip_prefix("/dev").unwrap().to_owned() + "/control";

        let fake_temperature_control =
            fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                ftemperaturecontrol::DeviceMarker,
            >(&dev, &control_path)
            .unwrap();

        let _status = fake_temperature_control.set_temperature_celsius(temperature).await.unwrap();
    }

    // Wait for the device to finish enumerating.
    pub async fn wait_for_device(&self, driver_path: &str) {
        let dev = self.realm_instance.as_ref().unwrap().driver_test_realm_connect_to_dev().unwrap();

        let path = driver_path.strip_prefix("/dev").unwrap().to_owned();

        device_watcher::recursive_wait(&dev, &path).await.unwrap();
    }
}

/// Ensures `destroy` was called on the TestEnv prior to it going out of scope. It would be nice to
/// do the work of `destroy` right here in `drop`, but we can't since `destroy` requires async.
impl Drop for TestEnv {
    fn drop(&mut self) {
        assert!(self.realm_instance.is_none(), "Must call destroy() to tear down test environment");
    }
}

/// Increases the time scale so Power Manager's interval-based operation runs faster for testing.
async fn set_fake_time_scale(realm_instance: &RealmInstance, scale: u32) {
    let fake_clock_control = realm_instance
        .root
        .connect_to_protocol_at_exposed_dir::<ftesting::FakeClockControlMarker>()
        .unwrap();

    fake_clock_control.pause().await.expect("failed to pause fake time: FIDL error");
    fake_clock_control
        .resume_with_increments(
            fuchsia_zircon::Duration::from_millis(1).into_nanos(),
            &ftesting::Increment::Determined(
                fuchsia_zircon::Duration::from_millis(scale.into()).into_nanos(),
            ),
        )
        .await
        .expect("failed to set fake time scale: FIDL error")
        .expect("failed to set fake time scale: protocol error");
}

/// Container to hold all of the mocks within the RealmInstance.
pub struct Mocks {
    pub activity_service: Arc<MockActivityService>,
    pub input_settings_service: Arc<MockInputSettingsService>,
    pub system_controller_service: Arc<MockSystemControllerService>,
}

/// Tests that Power Manager triggers a thermal reboot if the temperature sensor at the given path
/// reaches the provided temperature. The provided TestEnv is consumed because Power Manager
/// triggers a reboot.
pub async fn test_thermal_reboot(mut env: TestEnv, sensor_path: &str, temperature: f32) {
    let mut reboot_watcher = client_connectors::RebootWatcherClient::new(&env).await;

    // 1) set the mock temperature to the provided temperature
    // 2) verify the reboot watcher sees the reboot request for 'HighTemperature'
    // 3) verify the system controller receives the reboot request
    // 4) verify the Driver Manager receives the termination state request
    env.set_temperature(sensor_path, temperature).await;
    assert_eq!(reboot_watcher.get_reboot_reason().await, fpower::RebootReason::HighTemperature);
    env.mocks.system_controller_service.wait_for_shutdown_request().await;

    env.destroy().await;
}
