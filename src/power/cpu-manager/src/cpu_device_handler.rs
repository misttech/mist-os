// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::common_utils::result_debug_panic::ResultDebugPanic;
use crate::error::CpuManagerError;
use crate::log_if_err;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use crate::types::{Hertz, OperatingPoint, Volts};
use anyhow::{format_err, Context as _, Error};
use async_trait::async_trait;
use async_utils::event::Event as AsyncEvent;
use fidl_fuchsia_hardware_cpu_ctrl as fcpu_ctrl;
use fidl_fuchsia_io as fio;
use fuchsia_inspect::{self as inspect, NumericProperty, Property};
use serde_derive::Deserialize;
use serde_json as json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Node: CpuDeviceHandler
///
/// Summary: Provides an interface to interact with a CPU driver via
///          fuchsia.hardware.cpu_ctrl.Device.
///          Similar to CpuControlHandler in its logical management of a single CPU device, but is
///          more narrowly-scoped, as it does not administer thermal policy.
///
/// Handles Messages:
///     - GetOperatingPoint
///     - SetOperatingPoint
///     - GetCpuOperatingPoints
///
/// FIDL dependencies:
///     - fuchsia.hardware.cpu_ctrl.Device: used to query descriptions of CPU operating points
//
// TODO(https://fxbug.dev/42164952): Update summary when CpuControlHandler is removed.

/// Builder struct for CpuDeviceHandler.
pub struct CpuDeviceHandlerBuilder<'a> {
    /// Path to the CPU driver
    driver_path: String,

    cpu_ctrl_proxy: Option<fcpu_ctrl::DeviceProxy>,
    inspect_root: Option<&'a inspect::Node>,
}

impl<'a> CpuDeviceHandlerBuilder<'a> {
    pub fn new_from_json(json_data: json::Value, _nodes: &HashMap<String, Rc<dyn Node>>) -> Self {
        #[derive(Deserialize)]
        struct Config {
            driver_path: String,
        }

        #[derive(Deserialize)]
        struct JsonData {
            config: Config,
        }

        let data: JsonData = json::from_value(json_data).unwrap();
        Self::new_with_driver_path(data.config.driver_path)
    }

    /// Constructs a CpuDeviceHandlerBuilder from the provided CPU driver path
    pub fn new_with_driver_path(driver_path: String) -> Self {
        Self { driver_path: driver_path.clone(), cpu_ctrl_proxy: None, inspect_root: None }
    }

    /// Test-only interface to construct a builder with fake proxies
    #[cfg(test)]
    fn new_with_proxies(driver_path: String, cpu_ctrl_proxy: fcpu_ctrl::DeviceProxy) -> Self {
        Self { driver_path, cpu_ctrl_proxy: Some(cpu_ctrl_proxy), inspect_root: None }
    }

    /// Test-only interface to override the Inspect root
    #[cfg(test)]
    pub fn with_inspect_root(mut self, root: &'a inspect::Node) -> Self {
        self.inspect_root = Some(root);
        self
    }

    pub fn build(self) -> Result<Rc<CpuDeviceHandler>, Error> {
        // Optionally use the default inspect root node
        let inspect_root = self.inspect_root.unwrap_or(inspect::component::inspector().root());
        let inspect =
            InspectData::new(inspect_root, format!("CpuDeviceHandler ({})", self.driver_path));

        let mutable_inner = MutableInner { cpu_ctrl_proxy: self.cpu_ctrl_proxy, opps: Vec::new() };

        Ok(Rc::new(CpuDeviceHandler {
            init_done: AsyncEvent::new(),
            driver_path: self.driver_path,
            inspect,
            mutable_inner: RefCell::new(mutable_inner),
        }))
    }

    #[cfg(test)]
    pub async fn build_and_init(self) -> Rc<CpuDeviceHandler> {
        let node = self.build().unwrap();
        node.init().await.unwrap();
        node
    }
}

pub struct CpuDeviceHandler {
    /// Signalled after `init()` has completed. Used to ensure node doesn't process messages until
    /// its `init()` has completed.
    init_done: AsyncEvent,

    /// Path to the underlying CPU driver
    driver_path: String,

    /// A struct for managing Component Inspection data
    inspect: InspectData,

    /// Mutable inner state.
    mutable_inner: RefCell<MutableInner>,
}

impl CpuDeviceHandler {
    async fn handle_get_cpu_operating_points(&self) -> Result<MessageReturn, CpuManagerError> {
        fuchsia_trace::duration!(
            c"cpu_manager",
            c"CpuDeviceHandler::handle_get_cpu_operating_points",
            "driver" => self.driver_path.as_str()
        );

        self.init_done.wait().await;

        Ok(MessageReturn::GetCpuOperatingPoints(self.mutable_inner.borrow().opps.clone()))
    }

    async fn handle_get_operating_point(&self) -> Result<MessageReturn, CpuManagerError> {
        fuchsia_trace::duration!(
            c"cpu_manager",
            c"CpuDeviceHandler::handle_get_operating_point",
            "driver" => self.driver_path.as_str()
        );

        self.init_done.wait().await;

        let result = self.get_operating_point().await;
        log_if_err!(result, "Failed to get operating point");
        fuchsia_trace::instant!(
            c"cpu_manager",
            c"CpuDeviceHandler::get_operating_point_result",
            fuchsia_trace::Scope::Thread,
            "driver" => self.driver_path.as_str(),
            "result" => format!("{:?}", result).as_str()
        );

        match result {
            Ok(opp) => Ok(MessageReturn::GetOperatingPoint(opp)),
            Err(e) => {
                self.inspect.get_operating_point_errors.add(1);
                Err(CpuManagerError::GenericError(e))
            }
        }
    }

    async fn get_operating_point(&self) -> Result<u32, Error> {
        let proxy = &self.mutable_inner.borrow().cpu_ctrl_proxy;

        proxy
            .as_ref()
            .ok_or(format_err!("Missing driver_proxy"))
            .or_debug_panic()?
            .get_current_operating_point()
            .await
            .map_err(|e| {
                format_err!("{}: get_current_operating_point IPC failed: {}", self.name(), e)
            })
    }

    async fn handle_set_operating_point(
        &self,
        in_opp: u32,
    ) -> Result<MessageReturn, CpuManagerError> {
        fuchsia_trace::duration!(
            c"cpu_manager",
            c"CpuDeviceHandler::handle_set_operating_point",
            "driver" => self.driver_path.as_str(),
            "opp" => in_opp
        );

        self.init_done.wait().await;

        let result = self.set_operating_point(in_opp).await;
        log_if_err!(result, "Failed to set operating point");
        fuchsia_trace::instant!(
            c"cpu_manager",
            c"CpuDeviceHandler::set_operating_point_result",
            fuchsia_trace::Scope::Thread,
            "driver" => self.driver_path.as_str(),
            "result" => format!("{:?}", result).as_str()
        );

        match result {
            Ok(_) => {
                self.inspect.operating_point.set(in_opp.into());
                Ok(MessageReturn::SetOperatingPoint)
            }
            Err(e) => {
                self.inspect.set_operating_point_errors.add(1);
                self.inspect.last_set_operating_point_error.set(format!("{}", e).as_str());
                Err(CpuManagerError::GenericError(e))
            }
        }
    }

    async fn set_operating_point(&self, in_opp: u32) -> Result<(), Error> {
        let proxy = &self.mutable_inner.borrow().cpu_ctrl_proxy;

        // Make the FIDL call
        let _out_opp = proxy
            .as_ref()
            .ok_or(format_err!("Missing driver_proxy"))
            .or_debug_panic()?
            .set_current_operating_point(in_opp)
            .await
            .map_err(|e| {
                format_err!("{}: set_current_operating_point IPC failed: {}", self.name(), e)
            })?
            .map_err(|e| {
                format_err!(
                    "{}: set_current_operating_point driver returned error: {}",
                    self.name(),
                    e
                )
            })?;

        Ok(())
    }
}

struct MutableInner {
    cpu_ctrl_proxy: Option<fcpu_ctrl::DeviceProxy>,

    /// All opps provided by the underlying CPU driver
    opps: Vec<OperatingPoint>,
}

#[async_trait(?Send)]
impl Node for CpuDeviceHandler {
    fn name(&self) -> String {
        format!("CpuDeviceHandler ({})", self.driver_path)
    }

    /// Initializes internal state.
    ///
    /// Connects to the cpu-ctrl driver unless a proxy was already provided (in a test).
    async fn init(&self) -> Result<(), Error> {
        fuchsia_trace::duration!(c"cpu_manager", c"CpuDeviceHandler::init");

        // Connect to the cpu-ctrl driver. Typically this is None, but it may be set by tests.
        let cpu_ctrl_proxy = match &self.mutable_inner.borrow().cpu_ctrl_proxy {
            Some(p) => p.clone(),
            None => {
                const DEV_CLASS_CPUCTRL: &str = "/dev/class/cpu-ctrl/";

                let dir = fuchsia_fs::directory::open_in_namespace(
                    DEV_CLASS_CPUCTRL,
                    fio::OpenFlags::RIGHT_READABLE,
                )?;

                // TODO(https://fxbug.dev/42065064): Remove this requirement when the configuration
                // specifies the device more robustly than by its sequential number.
                let path = self.driver_path.strip_prefix(DEV_CLASS_CPUCTRL).ok_or_else(|| {
                    anyhow::anyhow!("driver_path={} not in {}", self.driver_path, DEV_CLASS_CPUCTRL)
                })?;
                device_watcher::wait_for_device_with(&dir, |info| {
                    (info.filename == path).then(|| {
                        fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                            fcpu_ctrl::DeviceMarker,
                        >(&dir, path)
                    })
                })
                .await??
            }
        };

        // Query the CPU opps
        let opps =
            get_opps(&self.driver_path, &cpu_ctrl_proxy).await.context("Failed to get CPU opps")?;
        validate_opps(&opps).context("Invalid CPU control params")?;
        self.inspect.record_opps(&opps);

        {
            let mut mutable_inner = self.mutable_inner.borrow_mut();
            mutable_inner.cpu_ctrl_proxy = Some(cpu_ctrl_proxy);
            mutable_inner.opps = opps;
        }

        self.init_done.signal();

        Ok(())
    }

    async fn handle_message(&self, msg: &Message) -> Result<MessageReturn, CpuManagerError> {
        match msg {
            Message::GetOperatingPoint => self.handle_get_operating_point().await,
            Message::SetOperatingPoint(opp) => self.handle_set_operating_point(*opp).await,
            Message::GetCpuOperatingPoints => self.handle_get_cpu_operating_points().await,
            _ => Err(CpuManagerError::Unsupported),
        }
    }
}

/// Retrieves all opps from the provided cpu_ctrl proxy.
async fn get_opps(
    cpu_driver_path: &str,
    cpu_ctrl_proxy: &fcpu_ctrl::DeviceProxy,
) -> Result<Vec<OperatingPoint>, Error> {
    fuchsia_trace::duration!(
        c"cpu_manager",
        c"CpuDeviceHandler::get_opps",
        "driver" => cpu_driver_path
    );

    // Query opp metadata from the cpu_ctrl interface. Each supported operating point has
    // accompanying opp metadata.
    let mut opps = Vec::new();

    let opp_count = cpu_ctrl_proxy
        .get_operating_point_count()
        .await
        .map_err(|e| {
            format_err!("{}: get_operating_point_count IPC failed: {}", cpu_driver_path, e)
        })?
        .map_err(|e| {
            format_err!("{}: get_operating_point_count returned error: {}", cpu_driver_path, e)
        })?;

    for i in 0..opp_count {
        let info = cpu_ctrl_proxy
            .get_operating_point_info(i)
            .await
            .map_err(|e| {
                format_err!("{}: get_operating_point_info IPC failed: {}", cpu_driver_path, e)
            })?
            .map_err(|e| {
                format_err!("{}: get_operating_point_info returned error: {}", cpu_driver_path, e)
            })?;

        opps.push(OperatingPoint {
            frequency: Hertz(info.frequency_hz as f64),
            voltage: Volts(info.voltage_uv as f64 / 1e6),
        });
    }

    Ok(opps)
}

/// Checks that the given list of opps satisfies the following conditions:
///  - Contains at least one element;
///  - Is primarily sorted by frequency;
///  - Is strictly secondarily sorted by voltage.
fn validate_opps(opps: &Vec<OperatingPoint>) -> Result<(), Error> {
    if opps.len() == 0 {
        anyhow::bail!("Must have at least one opp");
    } else if opps.len() > 1 {
        for pair in opps.as_slice().windows(2) {
            if pair[1].frequency > pair[0].frequency
                || (pair[1].frequency == pair[0].frequency && pair[1].voltage >= pair[0].voltage)
            {
                anyhow::bail!(
                    "opps must be primarily sorted by decreasing frequency and secondarily \
                    sorted by decreasing voltage; violated by {:?} and {:?}.",
                    pair[0],
                    pair[1]
                );
            }
        }
    }
    Ok(())
}

struct InspectData {
    // Nodes
    root_node: inspect::Node,

    operating_point: inspect::UintProperty,
    get_operating_point_errors: inspect::UintProperty,
    set_operating_point_errors: inspect::UintProperty,
    last_set_operating_point_error: inspect::StringProperty,
}

impl InspectData {
    fn new(parent: &inspect::Node, node_name: String) -> Self {
        // Create a local root node and properties
        let root_node = parent.create_child(node_name);

        let current_operating_point = root_node.create_child("current_operating_point");
        let operating_point = current_operating_point.create_uint("operating_point", 0);
        let get_operating_point_errors =
            current_operating_point.create_uint("get_operating_point_errors", 0);
        let set_operating_point_errors =
            current_operating_point.create_uint("set_operating_point_errors", 0);
        let last_set_operating_point_error =
            current_operating_point.create_string("last_set_operating_point_error", "");
        root_node.record(current_operating_point);

        InspectData {
            root_node,
            operating_point,
            get_operating_point_errors,
            set_operating_point_errors,
            last_set_operating_point_error,
        }
    }

    fn record_opps(&self, opps: &Vec<OperatingPoint>) {
        self.root_node.record_child("opps", |opps_node| {
            // Iterate opps in reverse order so that the Inspect nodes appear in the same order
            // as the vector (`record_child` inserts nodes at the head).
            for (i, opp) in opps.iter().enumerate().rev() {
                opps_node.record_child(format!("opp_{:02}", i), |node| {
                    node.record_double("voltage (V)", opp.voltage.0);
                    node.record_double("frequency (Hz)", opp.frequency.0);
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::TryStreamExt;
    use std::cell::Cell;

    /// Creates a fake fuchsia.hardware.cpu_ctrl.Device proxy
    fn setup_fake_cpu_ctrl_proxy(opps: Vec<OperatingPoint>) -> fcpu_ctrl::DeviceProxy {
        let operating_point = Rc::new(Cell::new(0));
        let operating_point_clone_1 = operating_point.clone();
        let operating_point_clone_2 = operating_point.clone();
        let get_operating_point = move || operating_point_clone_1.get();
        let set_operating_point = move |opp| {
            operating_point_clone_2.set(opp);
        };

        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fcpu_ctrl::DeviceMarker>().unwrap();

        fasync::Task::local(async move {
            while let Ok(req) = stream.try_next().await {
                match req {
                    Some(fcpu_ctrl::DeviceRequest::GetOperatingPointInfo { opp, responder }) => {
                        let index = opp as usize;
                        let result = if index < opps.len() {
                            Ok(fcpu_ctrl::CpuOperatingPointInfo {
                                frequency_hz: opps[index].frequency.0 as i64,
                                voltage_uv: (opps[index].voltage.0 * 1e6) as i64,
                            })
                        } else {
                            Err(zx::Status::NOT_SUPPORTED.into_raw())
                        };
                        let _ = responder.send(result.as_ref().map_err(|e| *e));
                    }
                    Some(fcpu_ctrl::DeviceRequest::GetOperatingPointCount { responder }) => {
                        let _ = responder.send(Ok(opps.len() as u32));
                    }
                    Some(fcpu_ctrl::DeviceRequest::GetCurrentOperatingPoint { responder }) => {
                        let _ = responder.send(get_operating_point());
                    }
                    Some(fcpu_ctrl::DeviceRequest::SetCurrentOperatingPoint {
                        requested_opp,
                        responder,
                    }) => {
                        set_operating_point(requested_opp as u32);
                        let _ = responder.send(Ok(requested_opp));
                    }
                    Some(other) => panic!("Unexpected request: {:?}", other),
                    None => break, // Stream terminates when client is dropped
                }
            }
        })
        .detach();

        proxy
    }

    async fn setup_simple_test_node(opps: Vec<OperatingPoint>) -> Rc<CpuDeviceHandler> {
        let builder = CpuDeviceHandlerBuilder::new_with_proxies(
            "fake_path".to_string(),
            setup_fake_cpu_ctrl_proxy(opps),
        );
        builder.build_and_init().await
    }

    /// Tests that an unsupported message is handled gracefully and an Unsupported error is returned
    #[fasync::run_singlethreaded(test)]
    async fn test_unsupported_msg() {
        let opps = vec![OperatingPoint { frequency: Hertz(1e9), voltage: Volts(1.0) }];
        let node = setup_simple_test_node(opps).await;
        match node.handle_message(&Message::GetCpuLoads).await {
            Err(CpuManagerError::Unsupported) => {}
            e => panic!("Unexpected return value: {:?}", e),
        }
    }

    /// Tests that the Get/SetOperatingPoint messages cause the node to call the appropriate
    /// device controller FIDL APIs.
    #[fasync::run_singlethreaded(test)]
    async fn test_operating_point() {
        let opps = vec![OperatingPoint { frequency: Hertz(1e9), voltage: Volts(1.0) }];
        let node = setup_simple_test_node(opps).await;

        // Send SetOperatingPoint message to set an opp of 1
        let commanded_operating_point = 1;
        match node
            .handle_message(&Message::SetOperatingPoint(commanded_operating_point))
            .await
            .unwrap()
        {
            MessageReturn::SetOperatingPoint => {}
            e => panic!("Unexpected return value: {:?}", e),
        }

        // Verify GetOperatingPoint reads back the same opp
        let received_operating_point =
            match node.handle_message(&Message::GetOperatingPoint).await.unwrap() {
                MessageReturn::GetOperatingPoint(opp) => opp,
                e => panic!("Unexpected return value: {:?}", e),
            };
        assert_eq!(commanded_operating_point, received_operating_point);

        // Send SetOperatingPoint message to set a opp of 2
        let commanded_operating_point = 2;
        match node
            .handle_message(&Message::SetOperatingPoint(commanded_operating_point))
            .await
            .unwrap()
        {
            MessageReturn::SetOperatingPoint => {}
            e => panic!("Unexpected return value: {:?}", e),
        }

        // Verify GetOperatingPoint reads back the same opp
        let received_operating_point =
            match node.handle_message(&Message::GetOperatingPoint).await.unwrap() {
                MessageReturn::GetOperatingPoint(opp) => opp,
                e => panic!("Unexpected return value: {:?}", e),
            };
        assert_eq!(commanded_operating_point, received_operating_point);
    }

    /// Tests that a GetCpuOperatingPoints message is handled properly.
    #[fasync::run_singlethreaded(test)]
    async fn test_get_cpu_operating_points() {
        let opps = vec![
            OperatingPoint { frequency: Hertz(1.4e9), voltage: Volts(0.9) },
            OperatingPoint { frequency: Hertz(1.3e9), voltage: Volts(0.8) },
            OperatingPoint { frequency: Hertz(1.2e9), voltage: Volts(0.7) },
        ];
        let node = setup_simple_test_node(opps.clone()).await;

        let received_opps =
            match node.handle_message(&Message::GetCpuOperatingPoints).await.unwrap() {
                MessageReturn::GetCpuOperatingPoints(v) => v,
                e => panic!("Unexpected return value: {:?}", e),
            };

        assert_eq!(opps, received_opps);
    }

    /// Tests that opp validation works as expected.
    #[fasync::run_singlethreaded(test)]
    async fn test_opp_validation() {
        // Primary sort by frequency is violated.
        let opps = vec![
            OperatingPoint { frequency: Hertz(1.5e9), voltage: Volts(1.0) },
            OperatingPoint { frequency: Hertz(1.6e9), voltage: Volts(1.0) },
        ];
        let builder = CpuDeviceHandlerBuilder::new_with_proxies(
            "fake_path".to_string(),
            setup_fake_cpu_ctrl_proxy(opps),
        );
        assert!(builder.build().unwrap().init().await.is_err());

        // Secondary sort by voltage is violated.
        let opps = vec![
            OperatingPoint { frequency: Hertz(1.5e9), voltage: Volts(1.0) },
            OperatingPoint { frequency: Hertz(1.5e9), voltage: Volts(1.1) },
        ];
        let builder = CpuDeviceHandlerBuilder::new_with_proxies(
            "fake_path".to_string(),
            setup_fake_cpu_ctrl_proxy(opps),
        );
        assert!(builder.build().unwrap().init().await.is_err());

        // Duplicated opp (detected as violation of secondary sort by voltage).
        let opps = vec![
            OperatingPoint { frequency: Hertz(1.5e9), voltage: Volts(1.0) },
            OperatingPoint { frequency: Hertz(1.5e9), voltage: Volts(1.0) },
        ];
        let builder = CpuDeviceHandlerBuilder::new_with_proxies(
            "fake_path".to_string(),
            setup_fake_cpu_ctrl_proxy(opps),
        );
        assert!(builder.build().unwrap().init().await.is_err());
    }

    /// Tests that Inspect data is populated as expected
    #[fasync::run_singlethreaded(test)]
    async fn test_inspect_data() {
        let opps = vec![
            OperatingPoint { frequency: Hertz(1.3e9), voltage: Volts(0.8) },
            OperatingPoint { frequency: Hertz(1.2e9), voltage: Volts(0.7) },
        ];

        let inspector = inspect::Inspector::default();
        let builder = CpuDeviceHandlerBuilder::new_with_proxies(
            "fake_path".to_string(),
            setup_fake_cpu_ctrl_proxy(opps.clone()),
        )
        .with_inspect_root(inspector.root());

        let _node = builder.build_and_init().await;

        assert_data_tree!(
            inspector,
            root: {
                "CpuDeviceHandler (fake_path)": {
                    "opps": {
                        opp_00: {
                            "frequency (Hz)": opps[0].frequency.0,
                            "voltage (V)": opps[0].voltage.0,
                        },
                        opp_01: {
                            "frequency (Hz)": opps[1].frequency.0,
                            "voltage (V)": opps[1].voltage.0,
                        },
                    },
                    "current_operating_point": contains {}
                }
            }
        );
    }
}
