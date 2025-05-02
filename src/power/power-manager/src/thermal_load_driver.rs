// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common_utils::get_current_timestamp;
use crate::log_if_err;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use crate::platform_metrics::PlatformMetric;
use crate::temperature_handler::{TemperatureFilter, TemperatureReadings};
use crate::types::{Celsius, Nanoseconds, Seconds, ThermalLoad};
use anyhow::{format_err, Error, Result};
use async_trait::async_trait;
use fuchsia_inspect::{self as inspect, Property};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::{StreamExt, TryFutureExt as _};
use log::*;
use serde_derive::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use {fuchsia_async as fasync, serde_json as json};

/// Node: ThermalLoadDriver
///
/// Summary: The purpose of this node is to determine and communicate the thermal load value(s) in
/// the system.
///
///   The behavior is unique from the ThermalPolicy node (which is also capable of determining and
///   communicating thermal load values) because it is purpose-built with the ability to source
///   multiple temperature sensors to determine their individual thermal load values. It also
///   differs from ThermalPolicy because it calculates ThermalLoad based on observed filtered
///   temperature with respect to configured per-sensor onset/reboot temperatures, whereas
///   ThermalPolicy uses integral errors (where "error" is the filtered temperature delta with
///   respect to a configured target temperature).
///
///   To do this, the node polls each of the provided temperature handler nodes at their specified
///   polling intervals. The temperature is used to calculate a per-sensor thermal load value (each
///   sensor may configure its own unique onset and reboot temperatures which define that sensor's
///   thermal load range). As the thermal load on a given sensor changes, the new load value is
///   communicated to each of the `thermal_load_notify_nodes` nodes.
///
/// Handles Messages: N/A
///
/// Sends Messages:
///   - SystemShutdown
///   - UpdateThermalLoad
///   - GetSensorName
///   - LogPlatformMetric
///
/// FIDL dependencies: N/A

// Record up to 3 hours of skin temperature data in 30 second intervals.
//
// Note, the duration covered by inspect can cover more than 3 hours of wall
// clock time because sampling is done on the runtime clock, which pauses
// during suspension, but timestamps are uptime.
const INSPECT_HISTORY_CAPACITY: usize = 360;
const INSPECT_HISTORY_POLL_INTERVAL: Seconds = Seconds(30.0);

pub struct ThermalLoadDriverBuilder<'a> {
    temperature_input_configs: Vec<TemperatureInputConfig>,
    system_shutdown_node: Rc<dyn Node>,
    platform_metrics_node: Rc<dyn Node>,
    thermal_load_notify_nodes: Vec<Rc<dyn Node>>,
    inspect_root: Option<&'a inspect::Node>,
}

impl ThermalLoadDriverBuilder<'_> {
    pub fn new_from_json(
        json_data: json::Value,
        nodes: &HashMap<String, Rc<dyn Node>>,
        structured_config: &power_manager_config_lib::Config,
    ) -> Self {
        #[derive(Deserialize)]
        struct JsonTemperatureInputConfig {
            temperature_handler_node_name: String,
            onset_temperature_c: f64,
            reboot_temperature_c: f64,
            poll_interval_s: f64,
            filter_time_constant_s: f64,
            #[serde(default)]
            log_for_test: bool,
        }

        #[derive(Deserialize)]
        struct Config {
            temperature_input_configs: Vec<JsonTemperatureInputConfig>,
        }

        #[derive(Deserialize)]
        struct Dependencies {
            system_shutdown_node: String,
            thermal_load_notify_nodes: Vec<String>,
            platform_metrics_node: String,
        }

        #[derive(Deserialize)]
        struct JsonData {
            config: Config,
            dependencies: Dependencies,
        }

        let data: JsonData = json::from_value(json_data).unwrap();
        Self {
            system_shutdown_node: nodes[&data.dependencies.system_shutdown_node].clone(),
            platform_metrics_node: nodes[&data.dependencies.platform_metrics_node].clone(),
            temperature_input_configs: data
                .config
                .temperature_input_configs
                .iter()
                .map(|config| TemperatureInputConfig {
                    temperature_handler_node: nodes[&config.temperature_handler_node_name].clone(),
                    onset_temperature: Celsius(config.onset_temperature_c),
                    reboot_temperature: Celsius(config.reboot_temperature_c),
                    poll_interval: Seconds(config.poll_interval_s),
                    filter_time_constant: Seconds(
                        if structured_config.disable_temperature_filter {
                            0.0
                        } else {
                            config.filter_time_constant_s
                        },
                    ),
                    log_for_test: config.log_for_test,
                })
                .collect(),
            thermal_load_notify_nodes: data
                .dependencies
                .thermal_load_notify_nodes
                .iter()
                .map(|node_name| nodes[node_name].clone())
                .collect(),
            inspect_root: None,
        }
    }

    pub async fn build(self) -> Result<Rc<ThermalLoadDriver>, Error> {
        // Optionally use the default inspect root node
        let inspect_root =
            self.inspect_root.unwrap_or_else(|| inspect::component::inspector().root());

        let node = Rc::new(ThermalLoadDriver {
            system_shutdown_node: self.system_shutdown_node,
            inspect: inspect_root.create_child("ThermalLoadDriver"),
            sensor_inspect_roots: RefCell::new(HashMap::new()),
            platform_metrics: self.platform_metrics_node,
            thermal_load_notify_nodes: self.thermal_load_notify_nodes,
            polling_tasks: RefCell::new(Vec::new()),
        });

        // Spawn polling tasks for each of the temperature input configs. The polling tasks are
        // collected into the node's `polling_tasks` field.
        for config in self.temperature_input_configs {
            node.create_polling_tasks(config).await?;
        }

        Ok(node)
    }
}

pub struct ThermalLoadDriver {
    /// Node that is used to initiate a system reboot when any temperature input source reaches
    /// their configured reboot temperature.
    system_shutdown_node: Rc<dyn Node>,

    /// Nodes that we notify when the thermal load value for any sensor has changed.
    thermal_load_notify_nodes: Vec<Rc<dyn Node>>,

    /// Parent Inspect node for the ThermalLoadDriver node (named as "ThermalLoadDriver"). Each
    /// polling task / temperature input source has a corresponding child node underneath this one.
    inspect: inspect::Node,

    /// Inspect node for each sensor. Each polling task will have child nodes underneath its
    /// respective root.
    sensor_inspect_roots: RefCell<HashMap<String, inspect::Node>>,

    /// Node that we'll notify with relevant platform metrics.
    platform_metrics: Rc<dyn Node>,

    /// Stores the Task objects that handle polling temperature sensors and taking appropriate
    /// action as their thermal loads change. The bulk of the ThermalLoadDriver's real work happens
    /// within these polling tasks. There exists a polling task for each individual temperature
    /// sensor that this node monitors.
    polling_tasks: RefCell<Vec<fasync::Task<()>>>,
}

impl ThermalLoadDriver {
    /// Creates a new polling task to begin running immediately.
    ///
    /// The function uses the provided TemperatureInputConfig to create and spawn a new polling
    /// task. The polling task is responsible for polling the temperature sensor at the configured
    /// interval, then taking appropriate action to determine thermal load and/or initiate thermal
    /// shutdown. The new polling task is added to the `polling_tasks` vector to retain ownership
    /// (instead of detaching the Task).
    async fn create_polling_tasks(self: &Rc<Self>, config: TemperatureInputConfig) -> Result<()> {
        // Query the TemperatureHandler to find out the driver's name. This sensor name is
        // used to identify the source of thermal load changes in the system.
        let sensor_name = self.query_sensor_name(&config.temperature_handler_node).await?;

        let temperature_input = Rc::new(TemperatureInput::new(&config));

        // Each sensor gets its own inspect root and polling tasks their own nodes.
        let sensor_inspect = self.inspect.create_child(&sensor_name);
        let input_inspect = TemperatureInputInspect::new(&sensor_inspect, &config);
        let history_inspect =
            TemperatureHistoryInspect::new(&sensor_inspect, INSPECT_HISTORY_CAPACITY);
        self.sensor_inspect_roots.borrow_mut().insert(sensor_name.clone(), sensor_inspect);

        self.polling_tasks.borrow_mut().extend(vec![
            self.create_thermal_load_polling_task(
                sensor_name.clone(),
                temperature_input.clone(),
                input_inspect,
                &config,
            ),
            self.create_temperature_logging_task(
                sensor_name,
                temperature_input,
                INSPECT_HISTORY_POLL_INTERVAL,
                history_inspect,
            ),
        ]);

        Ok(())
    }

    fn create_thermal_load_polling_task(
        self: &Rc<Self>,
        sensor_name: String,
        temperature_input: Rc<TemperatureInput>,
        inspect: TemperatureInputInspect,
        config: &TemperatureInputConfig,
    ) -> fasync::Task<()> {
        let this = self.clone();
        let log_for_test = config.log_for_test;
        let poll_interval = config.poll_interval;

        let polling_task = async move {
            let mut periodic_timer = fasync::Interval::new(poll_interval.into());

            // Enter the timer-based polling loop...
            let mut count: u32 = 0;
            while let Some(()) = periodic_timer.next().await {
                // Read a new temperature value. Errors are logged but the polling loop will
                // continue on the next iteration.
                let (_, temperature) = match temperature_input
                    .get_temperature(&sensor_name, &mut count, log_for_test)
                    .await
                {
                    Ok(load) => load,
                    Err(e) => {
                        error!(
                            "Failed to get updated temperature for {} (err = {})",
                            &sensor_name, e
                        );
                        continue;
                    }
                };

                // Compute the thermal load using the filtered temperature.
                let new_thermal_load =
                    temperature_input.temperature_to_thermal_load(temperature.filtered);

                fuchsia_trace::counter!(
                    c"power_manager",
                    c"ThermalLoadDriver thermal_load",
                    0,
                    "sensor" => sensor_name.as_str(),
                    "thermal_load" => new_thermal_load.0
                );

                inspect.log_thermal_load(new_thermal_load);

                if new_thermal_load >= ThermalLoad(100) {
                    log_if_err!(
                        this.initiate_thermal_shutdown().await,
                        "Failed to initiate thermal shutdown"
                    );
                } else {
                    log_if_err!(
                        this.send_message_to_many(
                            &this.thermal_load_notify_nodes,
                            &Message::UpdateThermalLoad(new_thermal_load, sensor_name.clone())
                        )
                        .await
                        .into_iter()
                        .collect::<Result<Vec<_>, _>>(),
                        "Failed to send thermal load update"
                    );
                }
            }

            Ok(())
        }
        .unwrap_or_else(|e: Error| error!("Failed to monitor sensor (err = {})", e));

        fasync::Task::local(polling_task)
    }

    fn create_temperature_logging_task(
        self: &Rc<Self>,
        sensor_name: String,
        temperature_input: Rc<TemperatureInput>,
        poll_interval: Seconds,
        mut inspect: TemperatureHistoryInspect,
    ) -> fasync::Task<()> {
        let polling_task = async move {
            let mut periodic_timer = fasync::Interval::new(poll_interval.into());

            let mut unused_count: u32 = 0;
            loop {
                let (time, temperature) = match temperature_input
                    .get_temperature(&sensor_name, &mut unused_count, false)
                    .await
                {
                    Ok(load) => load,
                    Err(e) => {
                        error!(
                            "Failed to get updated temperature for {} (err = {})",
                            &sensor_name, e
                        );
                        continue;
                    }
                };
                inspect.log_latest_temperature(time, temperature);

                // Wait at the end of the loop to ensure early temperatures are captured.
                if let None = periodic_timer.next().await {
                    error!("Failed to get wait for poll interval for {}, stopping", &sensor_name);
                    break;
                }
            }

            Ok(())
        }
        .unwrap_or_else(|e: Error| error!("Failed to log sensor temperature (err = {})", e));

        fasync::Task::local(polling_task)
    }

    /// Queries the provided TemperatureHandler node for its associated sensor name.
    async fn query_sensor_name(&self, temperature_handler: &Rc<dyn Node>) -> Result<String> {
        match self.send_message(temperature_handler, &Message::GetSensorName).await {
            Ok(MessageReturn::GetSensorName(name)) => Ok(name),
            _ => Err(format_err!("Failed to get sensor name for {}", temperature_handler.name())),
        }
    }

    /// Initiates a thermal shutdown.
    ///
    /// Sends a message to the SystemShutdown node to initiate a system shutdown due to extreme
    /// temperatures.
    async fn initiate_thermal_shutdown(&self) -> Result<()> {
        log_if_err!(
            self.send_message(
                &self.platform_metrics,
                &Message::LogPlatformMetric(PlatformMetric::ThrottlingResultShutdown)
            )
            .await,
            "Failed to send ThrottlingResultShutdown metric"
        );

        match self.send_message(&self.system_shutdown_node, &Message::HighTemperatureReboot).await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Describes the configuration for polling a single temperature sensor.
struct TemperatureInputConfig {
    /// TemperatureHandler node to be polled for temperature readings.
    temperature_handler_node: Rc<dyn Node>,

    /// Temperature at which thermal load will begin to increase. A temperature value of
    /// `onset_temperature` corresponds to a thermal load of 0. Beyond `onset_temperature`, thermal
    /// load will increase linearly with temperature until reaching `reboot_temperature.
    onset_temperature: Celsius,

    /// Temperature at which this node will initiate a system reboot due to critical temperature. A
    /// temperature value of `reboot_temperature` corresponds to a thermal load of 100.
    reboot_temperature: Celsius,

    /// Polling interval at which a new filtered temperature value will be read from the sensor.
    poll_interval: Seconds,

    /// Time constant to be used for filtering raw temperature readings. A value of 0 effectively
    /// disables filtering.
    filter_time_constant: Seconds,

    /// Indicate if we need to log extra info for testing purpose.
    log_for_test: bool,
}

/// Configuration and data source for a single temperature sensor.
struct TemperatureInput {
    /// Temperature filter instance to provide filtered temperature inputs.
    temperature_filter: TemperatureFilter,

    /// Temperature at which thermal load will begin to increase. A temperature value of
    /// `onset_temperature` corresponds to a thermal load of 0. Beyond `onset_temperature`, thermal
    /// load will increase linearly with temperature until reaching `reboot_temperature.
    onset_temperature: Celsius,

    /// Temperature at which this node will initiate a system reboot due to critical temperature. A
    /// temperature value of `reboot_temperature` corresponds to a thermal load of 100.
    reboot_temperature: Celsius,
}

impl TemperatureInput {
    fn new(config: &TemperatureInputConfig) -> Self {
        let temperature_filter =
            TemperatureFilter::new(config.temperature_handler_node.clone(), config.filter_time_constant);

        Self {
            temperature_filter,
            onset_temperature: config.onset_temperature,
            reboot_temperature: config.reboot_temperature,
        }
    }

    /// Gets the current temperature value for this temperature input and the time the value was
    /// measured.
    ///
    /// The function will first poll the temperature handler to retrieve the latest temperature.
    async fn get_temperature(
        &self,
        name: &String,
        call_count: &mut u32,
        log_for_test: bool,
    ) -> Result<(Nanoseconds, TemperatureReadings)> {
        let time = get_current_timestamp();
        self.temperature_filter.get_temperature(time).await.map(|temperature| {
            if log_for_test {
                if *call_count % 5 == 0 {
                    // The prefix LOG_FOR_TESTING may be used elsewhere so if this code is removed,
                    // it has to be added in other files for the same sensor. (see b/409073173).
                    info!("LOG_FOR_TESTING {}: {:?}", name, temperature);
                    *call_count = 0;
                }
                *call_count += 1;
            }

            (time, temperature)
        })
    }

    /// Converts temperature to thermal load as a function of temperature, onset temperature,
    /// and reboot temperature.
    fn temperature_to_thermal_load(&self, temperature: Celsius) -> ThermalLoad {
        if temperature < self.onset_temperature {
            ThermalLoad(0)
        } else if temperature > self.reboot_temperature {
            ThermalLoad(100)
        } else {
            ThermalLoad(
                ((temperature - self.onset_temperature).0
                    / (self.reboot_temperature - self.onset_temperature).0
                    * 100.0) as u32,
            )
        }
    }
}

#[async_trait(?Send)]
impl Node for ThermalLoadDriver {
    fn name(&self) -> String {
        "ThermalLoadDriver".to_string()
    }
}

struct TemperatureInputInspect {
    thermal_load_property: inspect::UintProperty,
}

impl TemperatureInputInspect {
    fn new(sensor_root: &inspect::Node, config: &TemperatureInputConfig) -> Self {
        let thermal_load_property = sensor_root.create_uint("thermal_load", 0);
        sensor_root.record_double("onset_temperature_c", config.onset_temperature.0);
        sensor_root.record_double("reboot_temperature_c", config.reboot_temperature.0);
        sensor_root.record_double("poll_interval_s", config.poll_interval.0);
        sensor_root.record_double("filter_time_constant_s", config.filter_time_constant.0);
        Self { thermal_load_property }
    }

    fn log_thermal_load(&self, load: ThermalLoad) {
        self.thermal_load_property.set(load.0.into());
    }
}

struct TemperatureHistoryInspect {
    _root: inspect::Node,
    latest_temperature: inspect::DoubleProperty,
    temperature_history: BoundedListNode,
}

impl TemperatureHistoryInspect {
    fn new(sensor_root: &inspect::Node, capacity: usize) -> Self {
        let _root = sensor_root.create_child("measurements");
        let latest_temperature = _root.create_double("latest_temperature_c", 0.0);
        let temperature_history =
            BoundedListNode::new(_root.create_child("temperature_history_c"), capacity);
        Self { _root, latest_temperature, temperature_history }
    }

    fn log_latest_temperature(&mut self, time: Nanoseconds, temp: TemperatureReadings) {
        let temp = temp.raw.0;
        self.latest_temperature.set(temp);
        self.temperature_history.add_entry(|node| {
            node.record_int("@time", time.0);
            node.record_double("temp", temp);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::test::mock_node::{create_dummy_node, MessageMatcher, MockNode, MockNodeMaker};
    use crate::{msg_eq, msg_ok_return};
    use diagnostics_assertions::assert_data_tree;
    use std::task::Poll::Ready;

    /// Tests that each node config file has proper configuration for ThermalLoadDriver entries. The
    /// test ensures that any TemperatureHandler nodes that are named inside the
    /// temperature_input_configs array are also listed under the "dependencies" object. This is
    /// important not only for tracking the true dependencies of a node, but also to be able to take
    /// advantage of the node dependency tests in power_manager.rs (e.g.
    /// test_each_node_config_file_dependency_ordering).
    #[test]
    pub fn test_config_files() -> Result<(), anyhow::Error> {
        crate::common_utils::test_each_node_config_file(|config_file| {
            let thermal_load_driver_nodes =
                config_file.iter().filter(|n| n["type"] == "ThermalLoadDriver");

            for node in thermal_load_driver_nodes {
                let temperature_handler_node_deps = node["dependencies"].as_object().unwrap()
                    ["temperature_handler_node_names"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|node_name| node_name.as_str().unwrap())
                    .collect::<Vec<_>>();
                let temperature_config_node_refs = node["config"]["temperature_input_configs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|temperature_config| {
                        temperature_config["temperature_handler_node_name"].as_str().unwrap()
                    })
                    .collect::<Vec<_>>();

                if temperature_handler_node_deps != temperature_config_node_refs {
                    return Err(format_err!(
                        "TemperatureHandler nodes listed under \"dependencies\" must match
                        the TemperatureHandler nodes referenced in \"temperature_input_configs\""
                    ));
                }
            }

            Ok(())
        })
    }

    /// Tests that well-formed node_config JSON can be used to create a new ThermalLoadDriverBuilder
    /// instance.
    #[fasync::run_singlethreaded(test)]
    async fn test_new_from_json() {
        let json_data = json::json!({
            "type": "ThermalLoadDriver",
            "name": "thermal_load_driver",
            "config": {
              "temperature_input_configs": [
                {
                  "temperature_handler_node_name": "temp_sensor_1",
                  "onset_temperature_c": 50.0,
                  "reboot_temperature_c": 80.0,
                  "poll_interval_s": 1.0,
                  "filter_time_constant_s": 5.0
                },
                {
                  "temperature_handler_node_name": "temp_sensor_2",
                  "onset_temperature_c": 60.0,
                  "reboot_temperature_c": 90.0,
                  "poll_interval_s": 1.0,
                  "filter_time_constant_s": 10.0
                }
              ]
            },
            "dependencies": {
              "platform_metrics_node": "platform_metrics",
              "system_shutdown_node": "shutdown",
              "thermal_load_notify_nodes": [
                "thermal_load_notify"
              ],
              "temperature_handler_node_names": [
                "temp_sensor_1",
                "temp_sensor_2"
              ]
            }
        });

        let mut nodes: HashMap<String, Rc<dyn Node>> = HashMap::new();
        nodes.insert("temp_sensor_1".to_string(), create_dummy_node());
        nodes.insert("temp_sensor_2".to_string(), create_dummy_node());
        nodes.insert("shutdown".to_string(), create_dummy_node());
        nodes.insert("thermal_load_notify".to_string(), create_dummy_node());
        nodes.insert("platform_metrics".to_string(), create_dummy_node());

        let structured_config = power_manager_config_lib::Config {
            enable_debug_service: false,
            node_config_path: String::new(),
            disable_temperature_filter: false,
        };
        let _ = ThermalLoadDriverBuilder::new_from_json(json_data, &nodes, &structured_config);
    }

    // Convenience function to add an UpdateThermalLoad message to a mock node's expected messages.
    fn expect_thermal_load(node: &Rc<MockNode>, thermal_load: u32, sensor_name: &str) {
        node.add_msg_response_pair((
            msg_eq!(UpdateThermalLoad(ThermalLoad(thermal_load), sensor_name.to_string())),
            msg_ok_return!(UpdateThermalLoad),
        ));
    }

    // Convenience struct for running the ThermalLoadDriver's thermal input tasks.
    struct NodeTestRunner {
        mock_temperature_nodes: Vec<Rc<MockNode>>,
        polling_tasks: Vec<fasync::Task<()>>,
        executor: fasync::TestExecutor,
    }

    impl NodeTestRunner {
        fn new(
            executor: fasync::TestExecutor,
            thermal_load_driver: Rc<ThermalLoadDriver>,
            mock_temperature_nodes: Vec<Rc<MockNode>>,
        ) -> Self {
            executor.set_fake_time(fasync::MonotonicInstant::from_nanos(0));
            let mut this = Self {
                executor,
                polling_tasks: thermal_load_driver.polling_tasks.take(),
                mock_temperature_nodes,
            };

            // Initialize the polling tasks (required so each polling task has a chance to set up
            // their timers)
            this.run_polling_tasks();

            this
        }

        // Runs all polling tasks (one for each temperature input) for one iteration (stopping at
        // their next timer).
        fn run_polling_tasks(&mut self) {
            // Run each polling task until stalled. The polling task will stall when it has
            // completed one iteration and is waiting on the next iteration timer.
            for task in self.polling_tasks.iter_mut() {
                let _ = self.executor.run_until_stalled(task);
            }
        }

        // Wakes each polling task's timer then runs the task until hitting the next timer.
        fn wake_and_run_polling_tasks(&mut self) {
            // Wake all pending timers and increment fake time accordingly
            for _ in 0..self.polling_tasks.len() {
                let wake_time = self.executor.wake_next_timer().unwrap();
                if wake_time > self.executor.now() {
                    self.executor.set_fake_time(wake_time);
                }
            }

            // There should not be any more pending timers
            assert_eq!(self.executor.wake_next_timer(), None);

            self.run_polling_tasks();
        }

        // Sets fake temperature values for each temperature input, then runs each polling task for
        // one iteration.
        fn iterate_with_temperature_inputs(&mut self, temperature_inputs: &[&[f64]]) {
            for (i, temperatures) in temperature_inputs.iter().enumerate() {
                for temperature in temperatures.iter() {
                    self.mock_temperature_nodes[i].add_msg_response_pair((
                        msg_eq!(ReadTemperature),
                        msg_ok_return!(ReadTemperature(Celsius(*temperature))),
                    ));
                }
            }

            self.wake_and_run_polling_tasks();
        }
    }

    /// Tests the ThermalLoadDriver's ability to monitor multiple temperature input sources,
    /// calculate their thermal loads independently, and send out thermal load change messages
    /// correctly.
    #[test]
    fn test_multiple_temperature_inputs() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create mock nodes
        let mut mock_maker = MockNodeMaker::new();
        let system_shutdown_node = create_dummy_node();
        let platform_metrics_node = create_dummy_node();
        let mock_thermal_load_receiver = mock_maker.make("mock_thermal_load_receiver", vec![]);
        let mock_temperature_handler_1 = mock_maker.make("temperature_handler_1", vec![]);
        let mock_temperature_handler_2 = mock_maker.make("temperature_handler_2", vec![]);

        // The ThermalLoadDriver asks for the driver name of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler_1.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver_1".to_string())),
        ));
        mock_temperature_handler_2.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver_2".to_string())),
        ));

        // The ThermalLoadDriver asks for the temperature of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler_1.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(10.0))),
        ));
        mock_temperature_handler_2.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(0.0))),
        ));

        // Create the ThermalLoadDriver node. The node has two temperature input sources that are
        // configured with differing onset/reboot temperatures, which adds a degree of testing for
        // the ThermalLoadDriver's ability to track thermal load for each source separately.
        let build_fut = ThermalLoadDriverBuilder {
            temperature_input_configs: vec![
                TemperatureInputConfig {
                    temperature_handler_node: mock_temperature_handler_1.clone(),
                    onset_temperature: Celsius(0.0),
                    reboot_temperature: Celsius(50.0),
                    poll_interval: Seconds(30.0),
                    filter_time_constant: Seconds(1.0),
                    log_for_test: false,
                },
                TemperatureInputConfig {
                    temperature_handler_node: mock_temperature_handler_2.clone(),
                    onset_temperature: Celsius(0.0),
                    reboot_temperature: Celsius(100.0),
                    poll_interval: Seconds(30.0),
                    filter_time_constant: Seconds(1.0),
                    log_for_test: false,
                },
            ],
            system_shutdown_node,
            platform_metrics_node,
            thermal_load_notify_nodes: vec![mock_thermal_load_receiver.clone()],
            inspect_root: None,
        }
        .build();

        futures::pin_mut!(build_fut);
        let node = match exec.run_until_stalled(&mut build_fut) {
            Ready(n) => n.unwrap(),
            _ => panic!("ThermalLoadDriver not built"),
        };

        // Create the test runner
        let mut node_runner = NodeTestRunner::new(
            exec,
            node,
            vec![mock_temperature_handler_1, mock_temperature_handler_2],
        );

        // Increase mock_1 temperature, expect a corresponding thermal load update
        expect_thermal_load(&mock_thermal_load_receiver, 20, "fake_driver_1");
        expect_thermal_load(&mock_thermal_load_receiver, 0, "fake_driver_2");
        node_runner.iterate_with_temperature_inputs(&[&[10.0, 10.0], &[0.0, 0.0]]);

        // Increase mock_2 temperature, expect a corresponding thermal load update
        expect_thermal_load(&mock_thermal_load_receiver, 20, "fake_driver_1");
        expect_thermal_load(&mock_thermal_load_receiver, 40, "fake_driver_2");
        node_runner.iterate_with_temperature_inputs(&[&[10.0, 10.0], &[40.0, 40.0]]);

        // Both temperatures remain constant, thermal load should still be sent
        expect_thermal_load(&mock_thermal_load_receiver, 20, "fake_driver_1");
        expect_thermal_load(&mock_thermal_load_receiver, 40, "fake_driver_2");
        node_runner.iterate_with_temperature_inputs(&[&[10.0, 10.0], &[40.0, 40.0]]);

        // Decrease temperature for both mocks, expect two corresponding thermal load updates
        expect_thermal_load(&mock_thermal_load_receiver, 10, "fake_driver_1");
        expect_thermal_load(&mock_thermal_load_receiver, 20, "fake_driver_2");
        node_runner.iterate_with_temperature_inputs(&[&[5.0, 5.0], &[20.0, 20.0]]);
    }

    /// Tests that when any of the temperature handler input nodes exceed `reboot_temperature`, then
    /// the ThermalLoadDriver node initiates a system reboot due to high temperature.
    #[test]
    fn test_trigger_shutdown() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create mock nodes
        let mut mock_maker = MockNodeMaker::new();
        let platform_metrics_node = create_dummy_node();
        let mock_temperature_handler = mock_maker.make("temperature_handler", vec![]);
        let mock_thermal_load_receiver = mock_maker.make("mock_thermal_load_receiver", vec![]);
        let system_shutdown_node = mock_maker.make(
            "mock_system_shutdown_node",
            vec![(msg_eq!(HighTemperatureReboot), msg_ok_return!(SystemShutdown))],
        );

        // The ThermalLoadDriver asks for the driver name of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver".to_string())),
        ));

        // The ThermalLoadDriver asks for the temperature of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(50.0))),
        ));

        let build_fut = ThermalLoadDriverBuilder {
            temperature_input_configs: vec![TemperatureInputConfig {
                temperature_handler_node: mock_temperature_handler.clone(),
                onset_temperature: Celsius(0.0),
                reboot_temperature: Celsius(50.0),
                poll_interval: Seconds(30.0),
                filter_time_constant: Seconds(1.0),
                log_for_test: false,
            }],
            system_shutdown_node,
            platform_metrics_node,
            thermal_load_notify_nodes: vec![mock_thermal_load_receiver],
            inspect_root: None,
        }
        .build();

        futures::pin_mut!(build_fut);
        let node = match exec.run_until_stalled(&mut build_fut) {
            Ready(n) => n.unwrap(),
            _ => panic!("ThermalLoadDriver not built"),
        };

        // Create the test runner
        let mut node_runner = NodeTestRunner::new(exec, node, vec![mock_temperature_handler]);

        // With a single iteration, this temperature will cause a system reboot
        node_runner.iterate_with_temperature_inputs(&[&[50.0, 50.0]]);

        // The system_shutdown_node mock verifies that the SystemShutdown message is sent by the
        // ThermalLoadDriver
    }

    /// Tests that the expected Inspect properties are present.
    #[test]
    fn test_inspect_data() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let inspector = inspect::Inspector::default();

        // Create mock nodes
        let mut mock_maker = MockNodeMaker::new();
        let platform_metrics_node = create_dummy_node();
        let mock_temperature_handler_1 = mock_maker.make("temperature_handler_1", vec![]);
        let mock_temperature_handler_2 = mock_maker.make("temperature_handler_2", vec![]);
        let mock_thermal_load_receiver = mock_maker.make("mock_thermal_load_receiver", vec![]);
        let system_shutdown_node = mock_maker.make("mock_system_shutdown_node", vec![]);

        // The ThermalLoadDriver asks for the driver name of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler_1.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver_1".to_string())),
        ));
        mock_temperature_handler_2.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver_2".to_string())),
        ));

        // The ThermalLoadDriver asks for the temperature of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler_1.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(10.0))),
        ));
        mock_temperature_handler_2.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(50.0))),
        ));

        let build_fut = ThermalLoadDriverBuilder {
            temperature_input_configs: vec![
                TemperatureInputConfig {
                    temperature_handler_node: mock_temperature_handler_1.clone(),
                    onset_temperature: Celsius(0.0),
                    reboot_temperature: Celsius(50.0),
                    poll_interval: Seconds(30.0),
                    filter_time_constant: Seconds(10.0),
                    log_for_test: false,
                },
                TemperatureInputConfig {
                    temperature_handler_node: mock_temperature_handler_2.clone(),
                    onset_temperature: Celsius(0.0),
                    reboot_temperature: Celsius(100.0),
                    poll_interval: Seconds(30.0),
                    filter_time_constant: Seconds(20.0),
                    log_for_test: false,
                },
            ],
            system_shutdown_node,
            platform_metrics_node,
            thermal_load_notify_nodes: vec![mock_thermal_load_receiver.clone()],
            inspect_root: Some(inspector.root()),
        }
        .build();

        futures::pin_mut!(build_fut);
        let node = match exec.run_until_stalled(&mut build_fut) {
            Ready(n) => n.unwrap(),
            _ => panic!("ThermalLoadDriver not built"),
        };

        // Create the test runner
        let mut node_runner = NodeTestRunner::new(
            exec,
            node,
            vec![mock_temperature_handler_1, mock_temperature_handler_2],
        );

        // Provide some fake temperature values that cause a thermal load change for both inputs
        expect_thermal_load(&mock_thermal_load_receiver, 20, "fake_driver_1");
        expect_thermal_load(&mock_thermal_load_receiver, 50, "fake_driver_2");
        node_runner.iterate_with_temperature_inputs(&[&[10.0, 10.0], &[50.0, 50.0]]);

        // Verify the expected thermal load values are present for both temperature inputs
        assert_data_tree!(
            inspector,
            root: {
                "ThermalLoadDriver": {
                    fake_driver_1: {
                        onset_temperature_c: 0.0,
                        reboot_temperature_c: 50.0,
                        poll_interval_s: 30.0,
                        filter_time_constant_s: 10.0,
                        thermal_load: 20u64,
                        measurements: {
                            latest_temperature_c: 10.0,
                            temperature_history_c:  {
                                    "0": {
                                        "@time": 0,
                                        "temp": 10.0,
                                    },
                                    "1": {
                                        "@time": 30000000000i64,
                                        "temp": 10.0,
                                    },
                            },
                        },
                    },
                    fake_driver_2: {
                        onset_temperature_c: 0.0,
                        reboot_temperature_c: 100.0,
                        poll_interval_s: 30.0,
                        filter_time_constant_s: 20.0,
                        thermal_load: 50u64,
                        measurements: {
                            latest_temperature_c: 50.0,
                            temperature_history_c:  {
                                    "0": {
                                        "@time": 0,
                                        "temp": 50.0,
                                    },
                                    "1": {
                                        "@time": 30000000000i64,
                                        "temp": 50.0,
                                    },
                            },
                        },
                    },
                },
            }
        );
    }

    /// Tests for correct platform metrics sent to the PlatformMetrics node.
    #[test]
    fn test_platform_metrics() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Create mock nodes
        let mut mock_maker = MockNodeMaker::new();
        let mock_platform_metrics = mock_maker.make("mock_platform_metrics", vec![]);
        let mock_temperature_handler = mock_maker.make("temperature_handler", vec![]);
        let mock_thermal_load_receiver = mock_maker.make("mock_thermal_load_receiver", vec![]);
        let mock_system_shutdown_node = mock_maker.make("mock_system_shutdown_node", vec![]);

        // The ThermalLoadDriver asks for the driver name of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler.add_msg_response_pair((
            msg_eq!(GetSensorName),
            msg_ok_return!(GetSensorName("fake_driver".to_string())),
        ));

        // The ThermalLoadDriver asks for the temperature of all TemperatureHandler nodes during
        // initialization
        mock_temperature_handler.add_msg_response_pair((
            msg_eq!(ReadTemperature),
            msg_ok_return!(ReadTemperature(Celsius(50.0))),
        ));

        let build_fut = ThermalLoadDriverBuilder {
            temperature_input_configs: vec![TemperatureInputConfig {
                temperature_handler_node: mock_temperature_handler.clone(),
                onset_temperature: Celsius(0.0),
                reboot_temperature: Celsius(50.0),
                poll_interval: Seconds(30.0),
                filter_time_constant: Seconds(1.0),
                log_for_test: false,
            }],
            system_shutdown_node: mock_system_shutdown_node.clone(),
            platform_metrics_node: mock_platform_metrics.clone(),
            thermal_load_notify_nodes: vec![mock_thermal_load_receiver.clone()],
            inspect_root: None,
        }
        .build();

        futures::pin_mut!(build_fut);
        let node = match exec.run_until_stalled(&mut build_fut) {
            Ready(n) => n.unwrap(),
            _ => panic!("ThermalLoadDriver not built"),
        };

        // Create the test runner
        let mut node_runner = NodeTestRunner::new(exec, node, vec![mock_temperature_handler]);

        // Verify if a sensor causes thermal shutdown then `ThrottlingResultShutdown` is sent
        mock_system_shutdown_node.add_msg_response_pair((
            msg_eq!(HighTemperatureReboot),
            msg_ok_return!(SystemShutdown),
        ));
        mock_platform_metrics.add_msg_response_pair((
            msg_eq!(LogPlatformMetric(PlatformMetric::ThrottlingResultShutdown)),
            msg_ok_return!(LogPlatformMetric),
        ));

        node_runner.iterate_with_temperature_inputs(&[&[50.0, 50.0]]);
    }
}
