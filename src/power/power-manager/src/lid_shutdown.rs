// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::common_utils::result_debug_panic::ResultDebugPanic;
use crate::error::PowerManagerError;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use crate::ok_or_default_err;
use crate::shutdown_request::ShutdownRequest;
use anyhow::{format_err, Context, Error};
use async_trait::async_trait;
use fidl_fuchsia_hardware_input::{ControllerMarker, DeviceMarker, DeviceProxy};
use fuchsia_inspect::{self as inspect, NumericProperty, Property};
use fuchsia_inspect_contrib::inspect_log;
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::channel::mpsc;
use futures::future::{FutureExt, LocalBoxFuture};
use futures::{StreamExt, TryStreamExt};
use serde_derive::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tracing::*;
use {fidl_fuchsia_hardware_hidbus as fhidbus, fuchsia_async as fasync, serde_json as json};

/// Node: LidShutdown
///
/// Summary: Responds to lid closed events from devices with a lid sensor by waiting for a report
///          using the input FIDL protocol.
///
/// Handles Messages: N/A
///
/// Sends Messages:
///     - SystemShutdown
///
/// FIDL dependencies:
///     - fuchsia.hardware.input: the node uses this protocol to wait on reports from the
///       lid device

/// The lid sensor is not a real HID device however this descriptor is defined in the lid driver
/// and will be used to send lid HID reports for any ACPI lid sensor.
const HID_LID_DESCRIPTOR: [u8; 9] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x80, // Usage (System Control)
    0xA1, 0x01, // Collection (Application)
    0x0A, 0xFF, 0x01, //   Usage (0x01FF, unique to lid reports)
];

// Lid closed HID report
const LID_CLOSED: u8 = 0x0;

static INPUT_DEVICES_DIRECTORY: &str = "/dev/class/input";

pub struct LidShutdownBuilder<'a> {
    driver_proxy: Option<DeviceProxy>,
    system_shutdown_node: Option<Rc<dyn Node>>,
    inspect_root: Option<&'a inspect::Node>,
}

impl<'a> LidShutdownBuilder<'a> {
    #[cfg(test)]
    pub fn new() -> Self {
        use crate::test::mock_node::create_dummy_node;

        LidShutdownBuilder {
            driver_proxy: None,
            system_shutdown_node: Some(create_dummy_node()),
            inspect_root: None,
        }
    }

    #[cfg(test)]
    pub fn driver_proxy(mut self, proxy: DeviceProxy) -> Self {
        self.driver_proxy = Some(proxy);
        self
    }

    #[cfg(test)]
    pub fn system_shutdown_node(mut self, node: Rc<dyn Node>) -> Self {
        self.system_shutdown_node = Some(node);
        self
    }

    #[cfg(test)]
    fn inspect_root(mut self, inspect_root: &'a inspect::Node) -> Self {
        self.inspect_root = Some(inspect_root);
        self
    }

    pub fn new_from_json(json_data: json::Value, nodes: &HashMap<String, Rc<dyn Node>>) -> Self {
        #[derive(Deserialize)]
        struct Dependencies {
            system_shutdown_node: String,
        }

        #[derive(Deserialize)]
        struct JsonData {
            dependencies: Dependencies,
        }

        let data: JsonData = json::from_value(json_data).unwrap();
        Self {
            system_shutdown_node: Some(nodes[&data.dependencies.system_shutdown_node].clone()),
            driver_proxy: None,
            inspect_root: None,
        }
    }

    pub fn build(self) -> Result<Rc<LidShutdown>, Error> {
        let system_shutdown_node = ok_or_default_err!(self.system_shutdown_node)?;

        // Use an mpsc channel to relay the `report_event` obtained in `init()` to `watch_lid_task`
        // created later in this function since `watch_lid_task` must be created here using the
        // node's Rc wrapper which won't be accessible in `init()`.
        let (report_event_sender, report_event_receiver) = mpsc::channel(1);

        let mutable_inner = MutableInner {
            driver_proxy: self.driver_proxy,
            report_event_sender,
            watch_lid_task: None,
        };

        // In tests use the provided inspect root node
        let inspect_root = self.inspect_root.unwrap_or(inspect::component::inspector().root());

        let node = Rc::new(LidShutdown {
            mutable_inner: RefCell::new(mutable_inner),
            system_shutdown_node,
            inspect: InspectData::new(inspect_root, "LidShutdown".to_string()),
        });

        let watch_lid_task = fasync::Task::local(node.clone().watch_lid(report_event_receiver));
        node.mutable_inner.borrow_mut().watch_lid_task = Some(watch_lid_task);

        Ok(node)
    }

    #[cfg(test)]
    pub async fn build_and_init(self) -> Rc<LidShutdown> {
        let node = self.build().unwrap();
        node.init().await.unwrap();
        node
    }
}

pub struct LidShutdown {
    /// Mutable inner state.
    mutable_inner: RefCell<MutableInner>,

    /// Node to provide the system shutdown functionality via the SystemShutdown message.
    system_shutdown_node: Rc<dyn Node>,

    /// A struct for managing Component Inspection data
    inspect: InspectData,
}

impl LidShutdown {
    fn watch_lid<'a>(
        self: Rc<Self>,
        mut report_event_receiver: mpsc::Receiver<zx::Event>,
    ) -> LocalBoxFuture<'a, ()> {
        async move {
            // Wait to receive `report_event` from `init()`, then continuously monitor it for lid
            // state changes
            if let Some(report_event) = report_event_receiver.next().await {
                loop {
                    self.watch_lid_inner(&report_event).await;
                }
            }
        }
        .boxed_local()
    }

    /// Watches the lid device for reports.
    async fn watch_lid_inner(&self, report_event: &zx::Event) {
        match fasync::OnSignals::new(report_event, zx::Signals::USER_0).await {
            Err(e) => error!("Could not wait for lid event: {:?}", e),
            _ => match self.check_report().await {
                Ok(()) => (),
                Err(e) => {
                    self.inspect.read_errors.add(1);
                    self.inspect.last_read_error.set(format!("{}", e).as_str());
                    error!("Failed to read lid report: {}", e)
                }
            },
        };
    }

    /// Reads the report from the lid sensor and sends shutdown signal if lid is closed.
    async fn check_report(&self) -> Result<(), Error> {
        // Extract `driver_proxy` from `mutable_inner`, returning an error (or asserting in debug)
        // if the proxy is missing
        let driver_proxy = self
            .mutable_inner
            .borrow()
            .driver_proxy
            .as_ref()
            .ok_or(format_err!("Missing driver_proxy"))
            .or_debug_panic()?
            .clone();

        match driver_proxy.read_report().await? {
            Err(status) => {
                return Err(format_err!("Error reading report {}", status));
            }
            Ok(fhidbus::Report { buf, .. }) => {
                let report = buf.expect("Error, expecting a report");

                if report.len() != 1 {
                    return Err(format_err!("Expected single byte report, found {:?}", report));
                }
                self.inspect.log_lid_report(format!("{:?}", report));
                let report = report[0];

                if report == LID_CLOSED {
                    info!("Lid closed. Shutting down...");
                    self.send_message(
                        &self.system_shutdown_node,
                        &Message::SystemShutdown(ShutdownRequest::PowerOff),
                    )
                    .await
                    .map_err(|e| format_err!("Failed to shut down the system: {:?}", e))?;
                }
                Ok(())
            }
        }
    }
}

struct MutableInner {
    /// Proxy to the lid sensor driver. Populated during `init()` unless previously supplied (in a
    /// test).
    driver_proxy: Option<DeviceProxy>,

    /// Task that monitors the lid sensor state and processes changes to that state. Requires being
    /// an Option because the Task has a reference to the node itself. Therefore, the node had to be
    /// created before the Task could be created, at which point `watch_lid_task` could be
    /// populated.
    watch_lid_task: Option<fasync::Task<()>>,

    /// Sender end to be used in `init()` once `report_event` is obtained from the lid driver. The
    /// event is sent to the receiver end in `watch_lid_task` where it will be used to monitor for
    /// lid sensor state changes.
    report_event_sender: mpsc::Sender<zx::Event>,
}

#[async_trait(?Send)]
impl Node for LidShutdown {
    fn name(&self) -> String {
        "LidShutdown".to_string()
    }

    /// Initializes internal state.
    ///
    /// Connects to the lid sensor driver unless a proxy was already provided (in a test).
    async fn init(&self) -> Result<(), Error> {
        fuchsia_trace::duration!(c"power_manager", c"LidShutdown::init");

        // Connect to the lid driver. Typically this is None, but it may be set by tests.
        let driver_proxy = match &self.mutable_inner.borrow().driver_proxy {
            Some(p) => p.clone(),
            None => find_lid_sensor().await?,
        };

        match driver_proxy
            .get_reports_event()
            .await
            .map_err(|_| format_err!("Could not get report event"))?
        {
            Ok(report_event) => {
                // Send `report_event` to be used by `watch_lid_task`
                self.mutable_inner.borrow_mut().report_event_sender.try_send(report_event)?;

                self.mutable_inner.borrow_mut().driver_proxy = Some(driver_proxy);

                Ok(())
            }
            Err(status) => Err(format_err!("Could not get report event {}", status)),
        }
    }

    async fn handle_message(&self, _msg: &Message) -> Result<MessageReturn, PowerManagerError> {
        Err(PowerManagerError::Unsupported)
    }
}

/// Checks all the input devices until the lid sensor is found.
async fn find_lid_sensor() -> Result<DeviceProxy, Error> {
    info!("Trying to find lid device");

    let dir_proxy = fuchsia_fs::directory::open_in_namespace(
        INPUT_DEVICES_DIRECTORY,
        fuchsia_fs::PERM_READABLE,
    )?;

    let mut watcher = fuchsia_fs::directory::Watcher::new(&dir_proxy).await?;

    while let Some(msg) = watcher.try_next().await? {
        match msg.event {
            fuchsia_fs::directory::WatchEvent::EXISTING
            | fuchsia_fs::directory::WatchEvent::ADD_FILE => {
                // Skip directory since we are looking for a device.
                if msg.filename == PathBuf::from(".") {
                    continue;
                }
                match open_sensor(&dir_proxy, &msg.filename).await {
                    Ok(device) => {
                        info!("Found lid device");
                        return Ok(device);
                    }
                    Err(e) => {
                        debug!("{:?} is not the lid device: {:?}", &msg.filename, e);
                    }
                }
            }
            _ => (),
        }
    }

    Err(format_err!("No lid device found"))
}

/// Opens the sensor's device file. Returns the device if the correct HID
/// report descriptor is found.
async fn open_sensor(
    directory: &fidl_fuchsia_io::DirectoryProxy,
    filename: &PathBuf,
) -> Result<DeviceProxy, Error> {
    let filename =
        filename.to_str().ok_or_else(|| format_err!("cannot convert {:?} to string", filename))?;
    let controller = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
        ControllerMarker,
    >(directory, filename)?;
    let (device, server_end) = fidl::endpoints::create_proxy::<DeviceMarker>()?;
    let () = controller.open_session(server_end)?;
    check_sensor(device).await
}

async fn check_sensor(device: DeviceProxy) -> Result<DeviceProxy, Error> {
    let device_descriptor =
        device.get_report_desc().await.context("Could not get device HID report descriptor")?;
    if device_descriptor.len() < HID_LID_DESCRIPTOR.len() {
        return Err(format_err!("Short HID header"));
    }
    let device_header = &device_descriptor[0..HID_LID_DESCRIPTOR.len()];
    if device_header == HID_LID_DESCRIPTOR {
        Ok(device)
    } else {
        Err(format_err!("Device is not lid sensor"))
    }
}

struct InspectData {
    lid_reports: RefCell<BoundedListNode>,
    read_errors: inspect::UintProperty,
    last_read_error: inspect::StringProperty,
}

impl InspectData {
    /// Number of inspect samples to store in the `lid_reports` BoundedListNode.
    // Store the last 60 lid reports
    const NUM_INSPECT_LID_REPORTS: usize = 60;

    fn new(parent: &inspect::Node, name: String) -> Self {
        // Create a local root node and properties
        let root = parent.create_child(name);
        let lid_reports = RefCell::new(BoundedListNode::new(
            root.create_child("lid_reports"),
            Self::NUM_INSPECT_LID_REPORTS,
        ));
        let read_errors = root.create_uint("read_lid_report_error_count", 0);
        let last_read_error = root.create_string("last_read_error", "");

        // Pass ownership of the new node to the parent node, otherwise it'll be dropped
        parent.record(root);

        InspectData { lid_reports, read_errors, last_read_error }
    }

    fn log_lid_report(&self, lid_report: String) {
        inspect_log!(self.lid_reports.borrow_mut(), lid_report: lid_report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::mock_node::create_dummy_node;
    use crate::utils::run_all_tasks_until_stalled::run_all_tasks_until_stalled;
    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, TreeAssertion};
    use fidl::endpoints::create_proxy_and_stream;
    use std::cell::Cell;
    use zx::{AsHandleRef, HandleBased};
    use {fidl_fuchsia_hardware_input as finput, fuchsia_async as fasync};

    const LID_OPEN: u8 = 0x1;

    const SHORT_HID_DESC: [u8; 8] = [0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x0A, 0xFF];
    const VALID_NOT_LID_HID_DESC: [u8; 9] = [0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x0A, 0xFF, 0x02];

    // Fake node to mock the ShutdownHandler node, providing a convenient function to wait on a
    // received shutdown message (`wait_for_shutdown`).
    struct FakeShutdownNode {
        shutdown_received_sender: RefCell<mpsc::Sender<()>>,
        shutdown_received_receiver: RefCell<mpsc::Receiver<()>>,
    }

    impl FakeShutdownNode {
        fn new() -> Rc<Self> {
            let (shutdown_received_sender, shutdown_received_receiver) = mpsc::channel(1);
            Rc::new(Self {
                shutdown_received_sender: RefCell::new(shutdown_received_sender),
                shutdown_received_receiver: RefCell::new(shutdown_received_receiver),
            })
        }

        async fn wait_for_shutdown(&self) {
            self.shutdown_received_receiver.borrow_mut().next().await.unwrap();
        }
    }

    #[async_trait(?Send)]
    impl Node for FakeShutdownNode {
        fn name(&self) -> String {
            "FakeShutdownNode".to_string()
        }

        async fn handle_message(&self, msg: &Message) -> Result<MessageReturn, PowerManagerError> {
            match msg {
                Message::SystemShutdown(ShutdownRequest::PowerOff) => {
                    self.shutdown_received_sender.borrow_mut().try_send(()).unwrap();
                    Ok(MessageReturn::SystemShutdown)
                }
                _ => panic!(),
            }
        }
    }

    // Fake driver representing the lid sensor driver. Provides a convenient `set_state` method for
    // simulating lid state changes. The driver handles requests for ReadReport and GetReportsEvent.
    // Each ReadReport responds with the `lid_report` most recently set by a call to `set_state`.
    struct FakeLidDriver {
        report_event: zx::Event,
        _server_task: fasync::Task<()>,
        proxy: DeviceProxy,
        lid_state: Rc<Cell<u8>>,
    }

    impl FakeLidDriver {
        fn new() -> Self {
            let lid_state = Rc::new(Cell::new(0));
            let lid_state_clone = lid_state.clone();

            let report_event = zx::Event::create();
            let report_event_clone =
                report_event.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

            let (proxy, mut stream) = create_proxy_and_stream::<DeviceMarker>().unwrap();

            let server_task = fasync::Task::local(async move {
                while let Ok(req) = stream.try_next().await {
                    match req {
                        Some(finput::DeviceRequest::ReadReport { responder }) => {
                            report_event_clone
                                .signal_handle(zx::Signals::USER_0, zx::Signals::NONE)
                                .expect("Failed to clear event signal");
                            let _ = responder.send(Ok(&fhidbus::Report {
                                buf: Some((&[lid_state_clone.get()]).to_vec()),
                                timestamp: Some(0),
                                ..Default::default()
                            }));
                        }
                        Some(finput::DeviceRequest::GetReportsEvent { responder }) => {
                            let _ = responder.send(Ok(report_event_clone
                                .duplicate_handle(zx::Rights::BASIC)
                                .unwrap()));
                        }
                        _ => assert!(false),
                    }
                }
            });

            Self { report_event, _server_task: server_task, proxy, lid_state }
        }

        fn set_state(&self, lid_state: u8) {
            self.lid_state.set(lid_state);
            self.report_event
                .signal_handle(zx::Signals::NONE, zx::Signals::USER_0)
                .expect("Failed to signal event");
        }

        fn proxy(&self) -> DeviceProxy {
            self.proxy.clone()
        }
    }

    // Creates a mock device proxy that receives GetReportDesc requests and returns the supplied
    // HID descriptor.
    fn mock_device_proxy(desc: Vec<u8>) -> DeviceProxy {
        let (device_proxy, mut stream) =
            create_proxy_and_stream::<DeviceMarker>().expect("Failed to create proxy and stream");
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    finput::DeviceRequest::GetReportDesc { responder } => {
                        let () = responder.send(&desc).unwrap();
                    }
                    req => panic!("{req:?}"),
                }
            }
        })
        .detach();
        device_proxy
    }

    /// Tests that well-formed configuration JSON does not panic the `new_from_json` function.
    #[fasync::run_singlethreaded(test)]
    async fn test_new_from_json() {
        let json_data = json::json!({
            "type": "LidShutdown",
            "name": "lid_shutdown",
            "dependencies": {
                "system_shutdown_node": "shutdown",
              },
        });

        let mut nodes: HashMap<String, Rc<dyn Node>> = HashMap::new();
        nodes.insert("shutdown".to_string(), create_dummy_node());
        let _ = LidShutdownBuilder::new_from_json(json_data, &nodes);
    }

    /// Tests that when the node receives a signal on its |report_event|, it checks for a lid
    /// report and, on reception of a lid closed report, it triggers a system shutdown.
    #[fasync::run_singlethreaded(test)]
    async fn test_triggered_shutdown() {
        let fake_lid_driver = FakeLidDriver::new();
        let shutdown_node = FakeShutdownNode::new();
        let _node = LidShutdownBuilder::new()
            .driver_proxy(fake_lid_driver.proxy())
            .system_shutdown_node(shutdown_node.clone())
            .build_and_init()
            .await;

        fake_lid_driver.set_state(LID_CLOSED);
        shutdown_node.wait_for_shutdown().await;
    }

    /// Tests that when the node receives a signal on its |report_event|, it checks for a lid
    /// report and, on reception of a lid open report, it does NOT trigger a system shutdown.
    #[test]
    fn test_event_handling() {
        let mut executor = fasync::TestExecutor::new();
        let fake_lid_driver = FakeLidDriver::new();
        let shutdown_node = FakeShutdownNode::new();
        let _node = executor.run_singlethreaded(
            LidShutdownBuilder::new()
                .driver_proxy(fake_lid_driver.proxy())
                .system_shutdown_node(shutdown_node.clone())
                .build_and_init(),
        );

        fake_lid_driver.set_state(LID_OPEN);

        let mut wait_for_shutdown_future = Box::pin(shutdown_node.wait_for_shutdown());
        assert!(executor.run_until_stalled(&mut wait_for_shutdown_future).is_pending());
    }

    /// Tests that an unsupported message is handled gracefully and an error is returned.
    #[fasync::run_singlethreaded(test)]
    async fn test_unsupported_msg() {
        let fake_lid_driver = FakeLidDriver::new();
        let node =
            LidShutdownBuilder::new().driver_proxy(fake_lid_driver.proxy()).build_and_init().await;

        match node.handle_message(&Message::ReadTemperature).await {
            Err(PowerManagerError::Unsupported) => {}
            e => panic!("Unexpected return value: {:?}", e),
        }
    }

    #[test]
    fn test_loop_is_not_blocked() {
        let mut executor = fasync::TestExecutor::new();
        let fake_lid_driver = FakeLidDriver::new();
        let _node = executor.run_singlethreaded(
            LidShutdownBuilder::new().driver_proxy(fake_lid_driver.proxy()).build_and_init(),
        );

        run_all_tasks_until_stalled(&mut executor);

        // If watch_lid() blocks waiting for the event, it will block forever (because the event
        // will never come), meaning that this test will time out. Otherwise run_until_stalled()
        // will return immediately and the test will pass.
    }

    /// Tests for the presence and correctness of dynamically-added inspect data
    #[test]
    fn test_inspect_data() {
        let mut executor = fasync::TestExecutor::new();
        let inspector = inspect::Inspector::default();
        let fake_lid_driver = FakeLidDriver::new();

        let _node = executor.run_singlethreaded(
            LidShutdownBuilder::new()
                .driver_proxy(fake_lid_driver.proxy())
                .inspect_root(inspector.root())
                .build_and_init(),
        );

        // The node will read the current temperature and log the sample into Inspect. Read enough
        // samples to test that the correct number of samples are logged and older ones are dropped.
        for _ in 0..InspectData::NUM_INSPECT_LID_REPORTS + 10 {
            fake_lid_driver.set_state(LID_OPEN);
            run_all_tasks_until_stalled(&mut executor);
        }

        let mut root = TreeAssertion::new("LidShutdown", false);
        let mut lid_reports = TreeAssertion::new("lid_reports", true);

        // Since we read 10 more samples than our limit allows, the first 10 should be dropped. So
        // test that the sample numbering starts at 10 and continues for the expected number of
        // samples.
        for i in 10..InspectData::NUM_INSPECT_LID_REPORTS + 10 {
            let mut sample_child = TreeAssertion::new(&i.to_string(), true);
            sample_child
                .add_property_assertion("lid_report", Box::new(format!("{:?}", [LID_OPEN])));
            sample_child
                .add_property_assertion("@time", Box::new(diagnostics_assertions::AnyProperty));
            lid_reports.add_child_assertion(sample_child);
        }

        root.add_child_assertion(lid_reports);
        assert_data_tree!(inspector, root: { root, });
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_short_hid_desc() {
        let mock_lid_proxy = mock_device_proxy(Vec::from(SHORT_HID_DESC));
        assert_matches!(check_sensor(mock_lid_proxy).await, Err(_));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_not_lid_hid_desc() {
        let mock_lid_proxy = mock_device_proxy(Vec::from(VALID_NOT_LID_HID_DESC));
        assert_matches!(check_sensor(mock_lid_proxy).await, Err(_));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_lid_hid_desc() {
        let mock_lid_proxy = mock_device_proxy(Vec::from(HID_LID_DESCRIPTOR));
        assert_matches!(check_sensor(mock_lid_proxy).await, Ok(_));
    }
}
