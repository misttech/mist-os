// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        autorepeater::Autorepeater, display_ownership::DisplayOwnership,
        focus_listener::FocusListener, input_device, input_handler, metrics,
    },
    anyhow::{format_err, Context, Error},
    fidl_fuchsia_input_injection, fidl_fuchsia_io as fio,
    focus_chain_provider::FocusChainProviderPublisher,
    fuchsia_async as fasync,
    fuchsia_fs::directory::{WatchEvent, Watcher},
    fuchsia_inspect::health::Reporter,
    fuchsia_inspect::NumericProperty,
    fuchsia_zircon as zx,
    futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
    futures::lock::Mutex,
    futures::{StreamExt, TryStreamExt},
    itertools::Itertools,
    metrics_registry::*,
    std::collections::HashMap,
    std::path::PathBuf,
    std::rc::Rc,
    std::sync::Arc,
};

type BoxedInputDeviceBinding = Box<dyn input_device::InputDeviceBinding>;

/// An [`InputDeviceBindingHashMap`] maps an input device to one or more InputDeviceBindings.
/// It expects filenames of the input devices seen in /dev/class/input-report (ex. "001") or
/// "injected_device" as keys.
pub type InputDeviceBindingHashMap = Arc<Mutex<HashMap<u32, Vec<BoxedInputDeviceBinding>>>>;

/// An input pipeline assembly.
///
/// Represents a partial stage of the input pipeline which accepts inputs through an asynchronous
/// sender channel, and emits outputs through an asynchronous receiver channel.  Use [new] to
/// create a new assembly.  Use [add_handler], or [add_all_handlers] to add the input pipeline
/// handlers to use.  When done, [InputPipeline::new] can be used to make a new input pipeline.
///
/// # Implementation notes
///
/// Internally, when a new [InputPipelineAssembly] is created with multiple [InputHandler]s, the
/// handlers are connected together using async queues.  This allows fully streamed processing of
/// input events, and also allows some pipeline stages to generate events spontaneously, i.e.
/// without an external stimulus.
pub struct InputPipelineAssembly {
    /// The top-level sender: send into this queue to inject an event into the input
    /// pipeline.
    sender: UnboundedSender<input_device::InputEvent>,
    /// The bottom-level receiver: any events that fall through the entire pipeline can
    /// be read from this receiver.  See [catch_unhandled] for a canned way to catch and
    /// log unhandled events.
    receiver: UnboundedReceiver<input_device::InputEvent>,
    /// The tasks that were instantiated as result of calling [new].  You *must*
    /// submit all the tasks to an executor to have them start.  Use [components] to
    /// get the tasks.  See [run] for a canned way to start these tasks.
    tasks: Vec<fuchsia_async::Task<()>>,

    /// The metrics logger.
    metrics_logger: metrics::MetricsLogger,
}

impl InputPipelineAssembly {
    /// Create a new but empty [InputPipelineAssembly]. Use [add_handler] or similar
    /// to add new handlers to it.
    pub fn new(metrics_logger: metrics::MetricsLogger) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let tasks = vec![];
        InputPipelineAssembly { sender, receiver, tasks, metrics_logger }
    }

    /// Adds another [input_handler::InputHandler] into the [InputPipelineAssembly]. The handlers
    /// are invoked in the order they are added, and successive handlers are glued together using
    /// unbounded queues.  Returns `Self` for chaining.
    pub fn add_handler(self, handler: Rc<dyn input_handler::InputHandler>) -> Self {
        let (sender, mut receiver, mut tasks, metrics_logger) = self.into_components();
        let metrics_logger_clone = metrics_logger.clone();
        let (next_sender, next_receiver) = mpsc::unbounded();
        let handler_name = handler.get_name();
        tasks.push(fasync::Task::local(async move {
            while let Some(event) = receiver.next().await {
                // Note: the `handler_name` _should not_ be used as ABI (e.g. referenced from
                // data processing scripts), as `handler_name` is not guaranteed to be consistent
                // between releases.
                let out_events = {
                    let _async_trace = fuchsia_trace::async_enter!(
                        fuchsia_trace::Id::new(),
                        "input",
                        "handle_input_event",
                        "name" => handler_name
                    );
                    handler.clone().handle_input_event(event).await
                };
                for out_event in out_events.into_iter() {
                    if let Err(e) = next_sender.unbounded_send(out_event) {
                        metrics_logger_clone.log_error(
                            InputPipelineErrorMetricDimensionEvent::InputPipelineCouldNotForwardEvent,
                            std::format!(
                                "could not forward event output from handler: {:?}: {:?}",
                                handler_name,
                                e));
                        // This is not a recoverable error, break here.
                        break;
                    }
                }
            }
            panic!("receive loop is not supposed to terminate for handler: {:?}", handler_name);
        }));
        receiver = next_receiver;
        InputPipelineAssembly { sender, receiver, tasks, metrics_logger }
    }

    /// Adds all handlers into the assembly in the order they appear in `handlers`.
    pub fn add_all_handlers(self, handlers: Vec<Rc<dyn input_handler::InputHandler>>) -> Self {
        handlers.into_iter().fold(self, |assembly, handler| assembly.add_handler(handler))
    }

    /// Adds the [DisplayOwnership] to the input pipeline.  The `display_ownership_event` is
    /// assumed to be the Scenic event used to report changes in display ownership, obtained
    /// by `fuchsia.ui.scenic/Scenic.GetDisplayOwnershipEvent`. This code has no way to check
    /// whether that invariant is upheld, so this is something that the user will need to
    /// ensure.
    pub fn add_display_ownership(
        self,
        display_ownership_event: zx::Event,
        input_handlers_node: &fuchsia_inspect::Node,
    ) -> InputPipelineAssembly {
        let (sender, autorepeat_receiver, mut tasks, metrics_logger) = self.into_components();
        let (autorepeat_sender, receiver) = mpsc::unbounded();
        let h = DisplayOwnership::new(display_ownership_event, input_handlers_node);
        let metrics_logger_clone = metrics_logger.clone();
        tasks.push(fasync::Task::local(async move {
            h.handle_input_events(autorepeat_receiver, autorepeat_sender)
                .await
                .map_err(|e| {
                    metrics_logger_clone.log_error(
                        InputPipelineErrorMetricDimensionEvent::InputPipelineDisplayOwnershipIsNotSupposedToTerminate,
                        std::format!(
                            "display ownership is not supposed to terminate - this is likely a problem: {:?}", e));
                }).unwrap();
        }));
        InputPipelineAssembly { sender, receiver, tasks, metrics_logger }
    }

    /// Adds the autorepeater into the input pipeline assembly.  The autorepeater
    /// is installed after any handlers that have been already added to the
    /// assembly.
    pub fn add_autorepeater(self, input_handlers_node: &fuchsia_inspect::Node) -> Self {
        let (sender, autorepeat_receiver, mut tasks, metrics_logger) = self.into_components();
        let (autorepeat_sender, receiver) = mpsc::unbounded();
        let metrics_logger_clone = metrics_logger.clone();
        let a = Autorepeater::new(autorepeat_receiver, input_handlers_node, metrics_logger.clone());
        tasks.push(fasync::Task::local(async move {
            a.run(autorepeat_sender)
                .await
                .map_err(|e| {
                    metrics_logger_clone.log_error(
                        InputPipelineErrorMetricDimensionEvent::InputPipelineAutorepeatRunningError,
                        std::format!("error while running autorepeater: {:?}", e),
                    );
                })
                .expect("autorepeater should never error out");
        }));
        InputPipelineAssembly { sender, receiver, tasks, metrics_logger }
    }

    /// Deconstructs the assembly into constituent components, used when constructing
    /// [InputPipeline].
    ///
    /// You should call [catch_unhandled] on the returned [async_channel::Receiver], and
    /// [run] on the returned [fuchsia_async::Tasks] (or supply own equivalents).
    fn into_components(
        self,
    ) -> (
        UnboundedSender<input_device::InputEvent>,
        UnboundedReceiver<input_device::InputEvent>,
        Vec<fuchsia_async::Task<()>>,
        metrics::MetricsLogger,
    ) {
        (self.sender, self.receiver, self.tasks, self.metrics_logger)
    }

    /// Adds a focus listener task into the input pipeline assembly.  The focus
    /// listener forwards focus chain changes to
    /// `fuchsia.ui.keyboard.focus.Controller` and watchers of
    /// `fuchsia.ui.focus.FocusChainProvider`.  It is required for the correct
    /// operation of the implementors of those protocols, e.g. `text_manager`.
    ///
    /// # Arguments:
    /// * `focus_chain_publisher`: to forward to other downstream watchers.
    ///
    /// # Requires:
    /// * `fuchsia.ui.views.FocusChainListenerRegistry`: to register for updates.
    /// * `fuchsia.ui.keyboard.focus.Controller`: to forward to text_manager.
    pub fn add_focus_listener(self, focus_chain_publisher: FocusChainProviderPublisher) -> Self {
        let (sender, receiver, mut tasks, metrics_logger) = self.into_components();
        let metrics_logger_clone = metrics_logger.clone();
        tasks.push(fasync::Task::local(async move {
            if let Ok(mut focus_listener) =
                FocusListener::new(focus_chain_publisher, metrics_logger_clone).map_err(|e| {
                    tracing::warn!(
                        "could not create focus listener, focus will not be dispatched: {:?}",
                        e
                    )
                })
            {
                // This will await indefinitely and process focus messages in a loop, unless there
                // is a problem.
                let _result = focus_listener
                    .dispatch_focus_changes()
                    .await
                    .map(|_| {
                        tracing::warn!(
                            "dispatch focus loop ended, focus will no longer be dispatched"
                        )
                    })
                    .map_err(|e| {
                        panic!("could not dispatch focus changes, this is a fatal error: {:?}", e)
                    });
            }
        }));
        InputPipelineAssembly { sender, receiver, tasks, metrics_logger }
    }
}

/// An [`InputPipeline`] manages input devices and propagates input events through input handlers.
///
/// On creation, clients declare what types of input devices an [`InputPipeline`] manages. The
/// [`InputPipeline`] will continuously detect new input devices of supported type(s).
///
/// # Example
/// ```
/// let ime_handler =
///     ImeHandler::new(scene_manager.session.clone(), scene_manager.compositor_id).await?;
/// let touch_handler = TouchHandler::new(
///     scene_manager.session.clone(),
///     scene_manager.compositor_id,
///     scene_manager.display_size
/// ).await?;
///
/// let assembly = InputPipelineAssembly::new()
///     .add_handler(Box::new(ime_handler)),
///     .add_handler(Box::new(touch_handler)),
/// let input_pipeline = InputPipeline::new(
///     vec![
///         input_device::InputDeviceType::Touch,
///         input_device::InputDeviceType::Keyboard,
///     ],
///     assembly,
/// );
/// input_pipeline.handle_input_events().await;
/// ```
pub struct InputPipeline {
    /// The entry point into the input handler pipeline. Incoming input events should
    /// be inserted into this async queue, and the input pipeline will ensure that they
    /// are propagated through all the input handlers in the appropriate sequence.
    pipeline_sender: UnboundedSender<input_device::InputEvent>,

    /// A clone of this sender is given to every InputDeviceBinding that this pipeline owns.
    /// Each InputDeviceBinding will send InputEvents to the pipeline through this channel.
    device_event_sender: UnboundedSender<input_device::InputEvent>,

    /// Receives InputEvents from all InputDeviceBindings that this pipeline owns.
    device_event_receiver: UnboundedReceiver<input_device::InputEvent>,

    /// The types of devices this pipeline supports.
    input_device_types: Vec<input_device::InputDeviceType>,

    /// The InputDeviceBindings bound to this pipeline.
    input_device_bindings: InputDeviceBindingHashMap,

    /// This node is bound to the lifetime of this InputPipeline.
    /// Inspect data will be dumped for this pipeline as long as it exists.
    inspect_node: fuchsia_inspect::Node,

    /// The metrics logger.
    metrics_logger: metrics::MetricsLogger,
}

impl InputPipeline {
    /// Does the work that is common to building an input pipeline, across
    /// the integration-test and production configurations.
    fn new_common(
        input_device_types: Vec<input_device::InputDeviceType>,
        assembly: InputPipelineAssembly,
        inspect_node: fuchsia_inspect::Node,
    ) -> Self {
        let (pipeline_sender, receiver, tasks, metrics_logger) = assembly.into_components();

        // Add properties to inspect node
        inspect_node.record_string("supported_input_devices", input_device_types.iter().join(", "));
        inspect_node.record_uint("handlers_registered", tasks.len() as u64);
        inspect_node.record_uint("handlers_healthy", tasks.len() as u64);

        // Add a stage that catches events which drop all the way down through the pipeline
        // and logs them.
        InputPipeline::catch_unhandled(receiver);

        // The tasks in the assembly are all unstarted.  Run them now.
        InputPipeline::run(tasks);

        let (device_event_sender, device_event_receiver) = futures::channel::mpsc::unbounded();
        let input_device_bindings: InputDeviceBindingHashMap = Arc::new(Mutex::new(HashMap::new()));
        InputPipeline {
            pipeline_sender,
            device_event_sender,
            device_event_receiver,
            input_device_types,
            input_device_bindings,
            inspect_node,
            metrics_logger,
        }
    }

    /// Creates a new [`InputPipeline`] for integration testing.
    /// Unlike a production input pipeline, this pipeline will not monitor
    /// `/dev/class/input-report` for devices.
    ///
    /// # Parameters
    /// - `input_device_types`: The types of devices the new [`InputPipeline`] will support.
    /// - `assembly`: The input handlers that the [`InputPipeline`] sends InputEvents to.
    pub fn new_for_test(
        input_device_types: Vec<input_device::InputDeviceType>,
        assembly: InputPipelineAssembly,
    ) -> Self {
        let inspector = fuchsia_inspect::Inspector::default();
        let root = inspector.root();
        let test_node = root.create_child("input_pipeline");
        Self::new_common(input_device_types, assembly, test_node)
    }

    /// Creates a new [`InputPipeline`] for production use.
    ///
    /// # Parameters
    /// - `input_device_types`: The types of devices the new [`InputPipeline`] will support.
    /// - `assembly`: The input handlers that the [`InputPipeline`] sends InputEvents to.
    /// - `inspect_node`: The root node for InputPipeline's Inspect tree
    pub fn new(
        input_device_types: Vec<input_device::InputDeviceType>,
        assembly: InputPipelineAssembly,
        inspect_node: fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<Self, Error> {
        let input_pipeline = Self::new_common(input_device_types, assembly, inspect_node);
        let input_device_types = input_pipeline.input_device_types.clone();
        let input_event_sender = input_pipeline.device_event_sender.clone();
        let input_device_bindings = input_pipeline.input_device_bindings.clone();
        let devices_node = input_pipeline.inspect_node.create_child("input_devices");
        fasync::Task::local(async move {
            // Watches the input device directory for new input devices. Creates new InputDeviceBindings
            // that send InputEvents to `input_event_receiver`.
            match async {
                let dir_proxy = fuchsia_fs::directory::open_in_namespace(
                    input_device::INPUT_REPORT_PATH,
                    fio::OpenFlags::empty(),
                )
                .with_context(|| format!("failed to open {}", input_device::INPUT_REPORT_PATH))?;
                let device_watcher =
                    Watcher::new(&dir_proxy).await.context("failed to create watcher")?;
                Self::watch_for_devices(
                    device_watcher,
                    dir_proxy,
                    input_device_types,
                    input_event_sender,
                    input_device_bindings,
                    &devices_node,
                    false, /* break_on_idle */
                    metrics_logger.clone(),
                )
                .await
                .context("failed to watch for devices")
            }
            .await
            {
                Ok(()) => {}
                Err(err) => {
                    // This error is usually benign in tests: it means that the setup does not
                    // support dynamic device discovery. Almost no tests support dynamic
                    // device discovery, and they also do not need those.
                    metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::InputPipelineUnableToWatchForNewInputDevices,
                        std::format!(
                            "Input pipeline is unable to watch for new input devices: {:?}",
                            err
                        ));
                }
            }
        })
        .detach();

        Ok(input_pipeline)
    }

    /// Gets the input device bindings.
    pub fn input_device_bindings(&self) -> &InputDeviceBindingHashMap {
        &self.input_device_bindings
    }

    /// Gets the input device sender: this is the channel that should be cloned
    /// and used for injecting events from the drivers into the input pipeline.
    pub fn input_event_sender(&self) -> &UnboundedSender<input_device::InputEvent> {
        &self.device_event_sender
    }

    /// Gets a list of input device types supported by this input pipeline.
    pub fn input_device_types(&self) -> &Vec<input_device::InputDeviceType> {
        &self.input_device_types
    }

    /// Forwards all input events into the input pipeline.
    pub async fn handle_input_events(mut self) {
        let metrics_logger_clone = self.metrics_logger.clone();
        while let Some(input_event) = self.device_event_receiver.next().await {
            if let Err(e) = self.pipeline_sender.unbounded_send(input_event) {
                metrics_logger_clone.log_error(
                    InputPipelineErrorMetricDimensionEvent::InputPipelineCouldNotForwardEventFromDriver,
                    std::format!("could not forward event from driver: {:?}", &e));
            }
        }

        metrics_logger_clone.log_error(
            InputPipelineErrorMetricDimensionEvent::InputPipelineStopHandlingEvents,
            "Input pipeline stopped handling input events.".to_string(),
        );
    }

    /// Watches the input report directory for new input devices. Creates InputDeviceBindings
    /// if new devices match a type in `device_types`.
    ///
    /// # Parameters
    /// - `device_watcher`: Watches the input report directory for new devices.
    /// - `dir_proxy`: The directory containing InputDevice connections.
    /// - `device_types`: The types of devices to watch for.
    /// - `input_event_sender`: The channel new InputDeviceBindings will send InputEvents to.
    /// - `bindings`: Holds all the InputDeviceBindings
    /// - `input_devices_node`: The parent node for all device bindings' inspect nodes.
    /// - `break_on_idle`: If true, stops watching for devices once all existing devices are handled.
    /// - `metrics_logger`: The metrics logger.
    ///
    /// # Errors
    /// If the input report directory or a file within it cannot be read.
    async fn watch_for_devices(
        mut device_watcher: Watcher,
        dir_proxy: fio::DirectoryProxy,
        device_types: Vec<input_device::InputDeviceType>,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        bindings: InputDeviceBindingHashMap,
        input_devices_node: &fuchsia_inspect::Node,
        break_on_idle: bool,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<(), Error> {
        // Add non-static properties to inspect node.
        let devices_discovered = input_devices_node.create_uint("devices_discovered", 0);
        let devices_connected = input_devices_node.create_uint("devices_connected", 0);
        while let Some(msg) = device_watcher.try_next().await? {
            if let Ok(filename) = msg.filename.into_os_string().into_string() {
                if filename == "." {
                    continue;
                }

                let pathbuf = PathBuf::from(filename.clone());
                match msg.event {
                    WatchEvent::EXISTING | WatchEvent::ADD_FILE => {
                        tracing::info!("found input device {}", filename);
                        devices_discovered.add(1);
                        let device_proxy =
                            input_device::get_device_from_dir_entry_path(&dir_proxy, &pathbuf)?;
                        add_device_bindings(
                            &device_types,
                            &filename,
                            device_proxy,
                            &input_event_sender,
                            &bindings,
                            filename.parse::<u32>().unwrap_or_default(),
                            input_devices_node,
                            Some(&devices_connected),
                            metrics_logger.clone(),
                        )
                        .await;
                    }
                    WatchEvent::IDLE => {
                        if break_on_idle {
                            break;
                        }
                    }
                    _ => (),
                }
            }
        }
        // Ensure inspect properties persist for debugging if device watch loop ends.
        if let Ok(devices_discovered_val) = devices_discovered.get() {
            std::mem::drop(devices_discovered);
            input_devices_node.record_uint("devices_discovered", devices_discovered_val);
        }
        if let Ok(devices_connected_val) = devices_connected.get() {
            std::mem::drop(devices_connected);
            input_devices_node.record_uint("devices_connected", devices_connected_val);
        }
        Err(format_err!("Input pipeline stopped watching for new input devices."))
    }

    /// Handles the incoming InputDeviceRegistryRequestStream.
    ///
    /// This method will end when the request stream is closed. If the stream closes with an
    /// error the error will be returned in the Result.
    ///
    /// **NOTE**: Only one stream is handled at a time. http://fxbug.dev/109722
    ///
    /// # Parameters
    /// - `stream`: The stream of InputDeviceRegistryRequests.
    /// - `device_types`: The types of devices to watch for.
    /// - `input_event_sender`: The channel new InputDeviceBindings will send InputEvents to.
    /// - `bindings`: Holds all the InputDeviceBindings associated with the InputPipeline.
    /// - `device_id`: The device id of the associated bindings.
    /// - `input_devices_node`: The parent node for all injected devices' inspect nodes.
    /// - `metrics_logger`: The metrics logger.
    pub async fn handle_input_device_registry_request_stream(
        mut stream: fidl_fuchsia_input_injection::InputDeviceRegistryRequestStream,
        device_types: &Vec<input_device::InputDeviceType>,
        input_event_sender: &UnboundedSender<input_device::InputEvent>,
        bindings: &InputDeviceBindingHashMap,
        device_id: u32,
        input_devices_node: &fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<(), Error> {
        while let Some(request) = stream
            .try_next()
            .await
            .context("Error handling input device registry request stream")?
        {
            match request {
                fidl_fuchsia_input_injection::InputDeviceRegistryRequest::Register {
                    device,
                    ..
                } => {
                    // Add a binding if the device is a type being tracked
                    let device_proxy = device.into_proxy().expect("Error getting device proxy.");

                    add_device_bindings(
                        device_types,
                        &format!("input-device-registry-{}", device_id),
                        device_proxy,
                        input_event_sender,
                        bindings,
                        device_id,
                        input_devices_node,
                        None,
                        metrics_logger.clone(),
                    )
                    .await;
                }
            }
        }

        Ok(())
    }

    /// Starts all tasks in an asynchronous executor.
    fn run(tasks: Vec<fuchsia_async::Task<()>>) {
        fasync::Task::local(async move {
            futures::future::join_all(tasks).await;
            panic!("Runner task is not supposed to terminate.")
        })
        .detach();
    }

    /// Installs a handler that will print a warning for each event that is received
    /// unhandled from this receiver.
    fn catch_unhandled(mut receiver: UnboundedReceiver<input_device::InputEvent>) {
        fasync::Task::local(async move {
            while let Some(event) = receiver.next().await {
                if event.handled == input_device::Handled::No {
                    tracing::warn!("unhandled input event: {:?}", &event);
                }
            }
            panic!("unhandled event catcher is not supposed to terminate.");
        })
        .detach();
    }
}

/// Adds `InputDeviceBinding`s to `bindings` for all `device_types` exposed by `device_proxy`.
///
/// # Parameters
/// - `device_types`: The types of devices to watch for.
/// - `device_proxy`: A proxy to the input device.
/// - `input_event_sender`: The channel new InputDeviceBindings will send InputEvents to.
/// - `bindings`: Holds all the InputDeviceBindings associated with the InputPipeline.
/// - `device_id`: The device id of the associated bindings.
/// - `input_devices_node`: The parent node for all device bindings' inspect nodes.
///
/// # Note
/// This will create multiple bindings, in the case where
/// * `device_proxy().get_descriptor()` returns a `fidl_fuchsia_input_report::DeviceDescriptor`
///   with multiple table fields populated, and
/// * multiple populated table fields correspond to device types present in `device_types`
///
/// This is used, for example, to support the Atlas touchpad. In that case, a single
/// node in `/dev/class/input-report` provides both a `fuchsia.input.report.MouseDescriptor` and
/// a `fuchsia.input.report.TouchDescriptor`.
async fn add_device_bindings(
    device_types: &Vec<input_device::InputDeviceType>,
    filename: &String,
    device_proxy: fidl_fuchsia_input_report::InputDeviceProxy,
    input_event_sender: &UnboundedSender<input_device::InputEvent>,
    bindings: &InputDeviceBindingHashMap,
    device_id: u32,
    input_devices_node: &fuchsia_inspect::Node,
    devices_connected: Option<&fuchsia_inspect::UintProperty>,
    metrics_logger: metrics::MetricsLogger,
) {
    let mut matched_device_types = vec![];
    if let Ok(descriptor) = device_proxy.get_descriptor().await {
        for device_type in device_types {
            if input_device::is_device_type(&descriptor, *device_type).await {
                matched_device_types.push(device_type);
                match devices_connected {
                    Some(dev_connected) => dev_connected.add(1),
                    None => (),
                };
            }
        }
        if matched_device_types.is_empty() {
            tracing::info!(
                "device {} did not match any supported device types: {:?}",
                filename,
                device_types
            );
            let device_node = input_devices_node.create_child(format!("{}_Unsupported", filename));
            let mut health = fuchsia_inspect::health::Node::new(&device_node);
            health.set_unhealthy("Unsupported device type.");
            device_node.record(health);
            input_devices_node.record(device_node);
            return;
        }
    } else {
        metrics_logger.clone().log_error(
            InputPipelineErrorMetricDimensionEvent::InputPipelineNoDeviceDescriptor,
            std::format!("cannot bind device {} without a device descriptor", filename),
        );
        return;
    }

    tracing::info!(
        "binding {} to device types: {}",
        filename,
        matched_device_types
            .iter()
            .fold(String::new(), |device_types_string, device_type| device_types_string
                + &format!("{:?}, ", device_type))
    );

    let mut new_bindings: Vec<BoxedInputDeviceBinding> = vec![];
    for device_type in matched_device_types {
        // Clone `device_proxy`, so that multiple bindings (e.g. a `MouseBinding` and a
        // `TouchBinding`) can read data from the same `/dev/class/input-report` node.
        //
        // There's no conflict in having multiple bindings read from the same node,
        // since:
        // * each binding will create its own `fuchsia.input.report.InputReportsReader`, and
        // * the device driver will copy each incoming report to each connected reader.
        //
        // This does mean that reports from the Atlas touchpad device get read twice
        // (by a `MouseBinding` and a `TouchBinding`), regardless of whether the device
        // is operating in mouse mode or touchpad mode.
        //
        // This hasn't been an issue because:
        // * Semantically: things are fine, because each binding discards irrelevant reports.
        //   (E.g. `MouseBinding` discards anything that isn't a `MouseInputReport`), and
        // * Performance wise: things are fine, because the data rate of the touchpad is low
        //   (125 HZ).
        //
        // If we add additional cases where bindings share an underlying `input-report` node,
        // we might consider adding a multiplexing binding, to avoid reading duplicate reports.
        let proxy = device_proxy.clone();
        let device_node = input_devices_node.create_child(format!("{}_{}", filename, device_type));
        match input_device::get_device_binding(
            *device_type,
            proxy,
            device_id,
            input_event_sender.clone(),
            device_node,
            metrics_logger.clone(),
        )
        .await
        {
            Ok(binding) => new_bindings.push(binding),
            Err(e) => {
                metrics_logger.log_error(
                    InputPipelineErrorMetricDimensionEvent::InputPipelineFailedToBind,
                    std::format!("failed to bind {} as {:?}: {}", filename, device_type, e),
                );
            }
        }
    }

    if !new_bindings.is_empty() {
        let mut bindings = bindings.lock().await;
        bindings.entry(device_id).or_insert(Vec::new()).extend(new_bindings);
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fake_input_device_binding,
        crate::input_device::{self, InputDeviceBinding},
        crate::mouse_binding,
        crate::mouse_model_database,
        crate::observe_fake_events_input_handler,
        crate::utils::Position,
        fidl::endpoints::{create_proxy, create_proxy_and_stream, create_request_stream},
        fuchsia_async as fasync,
        fuchsia_inspect::AnyProperty,
        fuchsia_zircon as zx,
        futures::channel::mpsc::UnboundedSender,
        futures::FutureExt,
        pretty_assertions::assert_eq,
        rand::Rng,
        std::collections::HashSet,
        vfs::{
            directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path,
            pseudo_directory, service as pseudo_fs_service,
        },
    };

    const COUNTS_PER_MM: u32 = 12;

    /// Returns the InputEvent sent over `sender`.
    ///
    /// # Parameters
    /// - `sender`: The channel to send the InputEvent over.
    fn send_input_event(
        sender: UnboundedSender<input_device::InputEvent>,
    ) -> input_device::InputEvent {
        let mut rng = rand::thread_rng();
        let offset = Position { x: rng.gen_range(0..10) as f32, y: rng.gen_range(0..10) as f32 };
        let input_event = input_device::InputEvent {
            device_event: input_device::InputDeviceEvent::Mouse(mouse_binding::MouseEvent::new(
                mouse_binding::MouseLocation::Relative(mouse_binding::RelativeLocation {
                    millimeters: Position {
                        x: offset.x / COUNTS_PER_MM as f32,
                        y: offset.y / COUNTS_PER_MM as f32,
                    },
                }),
                None, /* wheel_delta_v */
                None, /* wheel_delta_h */
                mouse_binding::MousePhase::Move,
                HashSet::new(),
                HashSet::new(),
                None, /* is_precision_scroll */
            )),
            device_descriptor: input_device::InputDeviceDescriptor::Mouse(
                mouse_binding::MouseDeviceDescriptor {
                    device_id: 1,
                    absolute_x_range: None,
                    absolute_y_range: None,
                    wheel_v_range: None,
                    wheel_h_range: None,
                    buttons: None,
                    counts_per_mm: COUNTS_PER_MM,
                },
            ),
            event_time: zx::Time::get_monotonic(),
            handled: input_device::Handled::No,
            trace_id: None,
        };
        match sender.unbounded_send(input_event.clone()) {
            Err(_) => assert!(false),
            _ => {}
        }

        input_event
    }

    /// Returns a MouseDescriptor on an InputDeviceRequest.
    ///
    /// # Parameters
    /// - `input_device_request`: The request to handle.
    fn handle_input_device_request(
        input_device_request: fidl_fuchsia_input_report::InputDeviceRequest,
    ) {
        match input_device_request {
            fidl_fuchsia_input_report::InputDeviceRequest::GetDescriptor { responder } => {
                let _ = responder.send(&fidl_fuchsia_input_report::DeviceDescriptor {
                    device_info: None,
                    mouse: Some(fidl_fuchsia_input_report::MouseDescriptor {
                        input: Some(fidl_fuchsia_input_report::MouseInputDescriptor {
                            movement_x: None,
                            movement_y: None,
                            scroll_v: None,
                            scroll_h: None,
                            buttons: Some(vec![0]),
                            position_x: None,
                            position_y: None,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    sensor: None,
                    touch: None,
                    keyboard: None,
                    consumer_control: None,
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    /// Tests that an input pipeline handles events from multiple devices.
    #[fasync::run_singlethreaded(test)]
    async fn multiple_devices_single_handler() {
        // Create two fake device bindings.
        let (device_event_sender, device_event_receiver) = futures::channel::mpsc::unbounded();
        let first_device_binding =
            fake_input_device_binding::FakeInputDeviceBinding::new(device_event_sender.clone());
        let second_device_binding =
            fake_input_device_binding::FakeInputDeviceBinding::new(device_event_sender.clone());

        // Create a fake input handler.
        let (handler_event_sender, mut handler_event_receiver) =
            futures::channel::mpsc::channel(100);
        let input_handler = observe_fake_events_input_handler::ObserveFakeEventsInputHandler::new(
            handler_event_sender,
        );

        // Build the input pipeline.
        let (sender, receiver, tasks, _) =
            InputPipelineAssembly::new(metrics::MetricsLogger::default())
                .add_handler(input_handler)
                .into_components();
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");
        let input_pipeline = InputPipeline {
            pipeline_sender: sender,
            device_event_sender,
            device_event_receiver,
            input_device_types: vec![],
            input_device_bindings: Arc::new(Mutex::new(HashMap::new())),
            inspect_node: test_node,
            metrics_logger: metrics::MetricsLogger::default(),
        };
        InputPipeline::catch_unhandled(receiver);
        InputPipeline::run(tasks);

        // Send an input event from each device.
        let first_device_event = send_input_event(first_device_binding.input_event_sender());
        let second_device_event = send_input_event(second_device_binding.input_event_sender());

        // Run the pipeline.
        fasync::Task::local(async {
            input_pipeline.handle_input_events().await;
        })
        .detach();

        // Assert the handler receives the events.
        let first_handled_event = handler_event_receiver.next().await;
        assert_eq!(first_handled_event, Some(first_device_event));

        let second_handled_event = handler_event_receiver.next().await;
        assert_eq!(second_handled_event, Some(second_device_event));
    }

    /// Tests that an input pipeline handles events through multiple input handlers.
    #[fasync::run_singlethreaded(test)]
    async fn single_device_multiple_handlers() {
        // Create two fake device bindings.
        let (device_event_sender, device_event_receiver) = futures::channel::mpsc::unbounded();
        let input_device_binding =
            fake_input_device_binding::FakeInputDeviceBinding::new(device_event_sender.clone());

        // Create two fake input handlers.
        let (first_handler_event_sender, mut first_handler_event_receiver) =
            futures::channel::mpsc::channel(100);
        let first_input_handler =
            observe_fake_events_input_handler::ObserveFakeEventsInputHandler::new(
                first_handler_event_sender,
            );
        let (second_handler_event_sender, mut second_handler_event_receiver) =
            futures::channel::mpsc::channel(100);
        let second_input_handler =
            observe_fake_events_input_handler::ObserveFakeEventsInputHandler::new(
                second_handler_event_sender,
            );

        // Build the input pipeline.
        let (sender, receiver, tasks, _) =
            InputPipelineAssembly::new(metrics::MetricsLogger::default())
                .add_handler(first_input_handler)
                .add_handler(second_input_handler)
                .into_components();
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");
        let input_pipeline = InputPipeline {
            pipeline_sender: sender,
            device_event_sender,
            device_event_receiver,
            input_device_types: vec![],
            input_device_bindings: Arc::new(Mutex::new(HashMap::new())),
            inspect_node: test_node,
            metrics_logger: metrics::MetricsLogger::default(),
        };
        InputPipeline::catch_unhandled(receiver);
        InputPipeline::run(tasks);

        // Send an input event.
        let input_event = send_input_event(input_device_binding.input_event_sender());

        // Run the pipeline.
        fasync::Task::local(async {
            input_pipeline.handle_input_events().await;
        })
        .detach();

        // Assert both handlers receive the event.
        let first_handler_event = first_handler_event_receiver.next().await;
        assert_eq!(first_handler_event, Some(input_event.clone()));
        let second_handler_event = second_handler_event_receiver.next().await;
        assert_eq!(second_handler_event, Some(input_event));
    }

    /// Tests that a single mouse device binding is created for the one input device in the
    /// input report directory.
    #[fasync::run_singlethreaded(test)]
    async fn watch_devices_one_match_exists() {
        // Create a file in a pseudo directory that represents an input device.
        let mut count: i8 = 0;
        let dir = pseudo_directory! {
            "001" => pseudo_fs_service::host(
                move |mut request_stream: fidl_fuchsia_input_report::InputDeviceRequestStream| {
                    async move {
                        while count < 3 {
                            if let Some(input_device_request) =
                                request_stream.try_next().await.unwrap()
                            {
                                handle_input_device_request(input_device_request);
                                count += 1;
                            }
                        }

                    }.boxed()
                },
            )
        };

        // Create a Watcher on the pseudo directory.
        let pseudo_dir_clone = dir.clone();
        let (dir_proxy_for_watcher, dir_server_for_watcher) =
            create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end_for_watcher = dir_server_for_watcher.into_channel().into();
        let scope_for_watcher = ExecutionScope::new();
        dir.open(scope_for_watcher, fio::OpenFlags::empty(), Path::dot(), server_end_for_watcher);
        let device_watcher = Watcher::new(&dir_proxy_for_watcher).await.unwrap();

        // Get a proxy to the pseudo directory for the input pipeline. The input pipeline uses this
        // proxy to get connections to input devices.
        let (dir_proxy_for_pipeline, dir_server_for_pipeline) =
            create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end_for_pipeline = dir_server_for_pipeline.into_channel().into();
        let scope_for_pipeline = ExecutionScope::new();
        pseudo_dir_clone.open(
            scope_for_pipeline,
            fio::OpenFlags::empty(),
            Path::dot(),
            server_end_for_pipeline,
        );

        let (input_event_sender, _input_event_receiver) = futures::channel::mpsc::unbounded();
        let bindings: InputDeviceBindingHashMap = Arc::new(Mutex::new(HashMap::new()));
        let supported_device_types = vec![input_device::InputDeviceType::Mouse];

        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");
        test_node.record_string(
            "supported_input_devices",
            supported_device_types.clone().iter().join(", "),
        );
        let input_devices = test_node.create_child("input_devices");
        // Assert that inspect tree is initialized with no devices.
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_pipeline: {
                supported_input_devices: "Mouse",
                input_devices: {}
            }
        });

        let _ = InputPipeline::watch_for_devices(
            device_watcher,
            dir_proxy_for_pipeline,
            supported_device_types,
            input_event_sender,
            bindings.clone(),
            &input_devices,
            true, /* break_on_idle */
            metrics::MetricsLogger::default(),
        )
        .await;

        // Assert that one mouse device with accurate device id was found.
        let bindings_hashmap = bindings.lock().await;
        assert_eq!(bindings_hashmap.len(), 1);
        let bindings_vector = bindings_hashmap.get(&1);
        assert!(bindings_vector.is_some());
        assert_eq!(bindings_vector.unwrap().len(), 1);
        let boxed_mouse_binding = bindings_vector.unwrap().get(0);
        assert!(boxed_mouse_binding.is_some());
        assert_eq!(
            boxed_mouse_binding.unwrap().get_device_descriptor(),
            input_device::InputDeviceDescriptor::Mouse(mouse_binding::MouseDeviceDescriptor {
                device_id: 1,
                absolute_x_range: None,
                absolute_y_range: None,
                wheel_v_range: None,
                wheel_h_range: None,
                buttons: Some(vec![0]),
                counts_per_mm: mouse_model_database::db::DEFAULT_COUNTS_PER_MM,
            })
        );

        // Assert that inspect tree reflects new device discovered and connected.
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_pipeline: {
                supported_input_devices: "Mouse",
                input_devices: {
                    devices_discovered: 1u64,
                    devices_connected: 1u64,
                    "001_Mouse": contains {
                        reports_received_count: 0u64,
                        reports_filtered_count: 0u64,
                        events_generated: 0u64,
                        last_received_timestamp_ns: 0u64,
                        last_generated_timestamp_ns: 0u64,
                        "fuchsia.inspect.Health": {
                            status: "OK",
                            // Timestamp value is unpredictable and not relevant in this context,
                            // so we only assert that the property is present.
                            start_timestamp_nanos: AnyProperty
                        },
                    }
                }
            }
        });
    }

    /// Tests that no device bindings are created because the input pipeline looks for keyboard devices
    /// but only a mouse exists.
    #[fasync::run_singlethreaded(test)]
    async fn watch_devices_no_matches_exist() {
        // Create a file in a pseudo directory that represents an input device.
        let mut count: i8 = 0;
        let dir = pseudo_directory! {
            "001" => pseudo_fs_service::host(
                move |mut request_stream: fidl_fuchsia_input_report::InputDeviceRequestStream| {
                    async move {
                        while count < 1 {
                            if let Some(input_device_request) =
                                request_stream.try_next().await.unwrap()
                            {
                                handle_input_device_request(input_device_request);
                                count += 1;
                            }
                        }

                    }.boxed()
                },
            )
        };

        // Create a Watcher on the pseudo directory.
        let pseudo_dir_clone = dir.clone();
        let (dir_proxy_for_watcher, dir_server_for_watcher) =
            create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end_for_watcher = dir_server_for_watcher.into_channel().into();
        let scope_for_watcher = ExecutionScope::new();
        dir.open(scope_for_watcher, fio::OpenFlags::empty(), Path::dot(), server_end_for_watcher);
        let device_watcher = Watcher::new(&dir_proxy_for_watcher).await.unwrap();

        // Get a proxy to the pseudo directory for the input pipeline. The input pipeline uses this
        // proxy to get connections to input devices.
        let (dir_proxy_for_pipeline, dir_server_for_pipeline) =
            create_proxy::<fio::DirectoryMarker>().unwrap();
        let server_end_for_pipeline = dir_server_for_pipeline.into_channel().into();
        let scope_for_pipeline = ExecutionScope::new();
        pseudo_dir_clone.open(
            scope_for_pipeline,
            fio::OpenFlags::empty(),
            Path::dot(),
            server_end_for_pipeline,
        );

        let (input_event_sender, _input_event_receiver) = futures::channel::mpsc::unbounded();
        let bindings: InputDeviceBindingHashMap = Arc::new(Mutex::new(HashMap::new()));
        let supported_device_types = vec![input_device::InputDeviceType::Keyboard];

        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");
        test_node.record_string(
            "supported_input_devices",
            supported_device_types.clone().iter().join(", "),
        );
        let input_devices = test_node.create_child("input_devices");
        // Assert that inspect tree is initialized with no devices.
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_pipeline: {
                supported_input_devices: "Keyboard",
                input_devices: {}
            }
        });

        let _ = InputPipeline::watch_for_devices(
            device_watcher,
            dir_proxy_for_pipeline,
            supported_device_types,
            input_event_sender,
            bindings.clone(),
            &input_devices,
            true, /* break_on_idle */
            metrics::MetricsLogger::default(),
        )
        .await;

        // Assert that no devices were found.
        let bindings = bindings.lock().await;
        assert_eq!(bindings.len(), 0);

        // Assert that inspect tree reflects new device discovered, but not connected.
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_pipeline: {
                supported_input_devices: "Keyboard",
                input_devices: {
                    devices_discovered: 1u64,
                    devices_connected: 0u64,
                    "001_Unsupported": {
                        "fuchsia.inspect.Health": {
                            status: "UNHEALTHY",
                            message: "Unsupported device type.",
                            // Timestamp value is unpredictable and not relevant in this context,
                            // so we only assert that the property is present.
                            start_timestamp_nanos: AnyProperty
                        },
                    }
                }
            }
        });
    }

    /// Tests that a single keyboard device binding is created for the input device registered
    /// through InputDeviceRegistry.
    #[fasync::run_singlethreaded(test)]
    async fn handle_input_device_registry_request_stream() {
        let (input_device_registry_proxy, input_device_registry_request_stream) =
            create_proxy_and_stream::<fidl_fuchsia_input_injection::InputDeviceRegistryMarker>()
                .unwrap();
        let (input_device_client_end, mut input_device_request_stream) =
            create_request_stream::<fidl_fuchsia_input_report::InputDeviceMarker>().unwrap();

        let device_types = vec![input_device::InputDeviceType::Mouse];
        let (input_event_sender, _input_event_receiver) = futures::channel::mpsc::unbounded();
        let bindings: InputDeviceBindingHashMap = Arc::new(Mutex::new(HashMap::new()));

        // Handle input device requests.
        let mut count: i8 = 0;
        fasync::Task::local(async move {
            // Register a device.
            let _ = input_device_registry_proxy.register(input_device_client_end);

            while count < 3 {
                if let Some(input_device_request) =
                    input_device_request_stream.try_next().await.unwrap()
                {
                    handle_input_device_request(input_device_request);
                    count += 1;
                }
            }

            // End handle_input_device_registry_request_stream() by taking the event stream.
            input_device_registry_proxy.take_event_stream();
        })
        .detach();

        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");

        // Start listening for InputDeviceRegistryRequests.
        let bindings_clone = bindings.clone();
        let _ = InputPipeline::handle_input_device_registry_request_stream(
            input_device_registry_request_stream,
            &device_types,
            &input_event_sender,
            &bindings_clone,
            0,
            &test_node,
            metrics::MetricsLogger::default(),
        )
        .await;

        // Assert that a device was registered.
        let bindings = bindings.lock().await;
        assert_eq!(bindings.len(), 1);
    }

    // Tests that correct properties are added to inspect node when InputPipeline is created.
    #[fasync::run_singlethreaded(test)]
    async fn check_inspect_node_has_correct_properties() {
        let device_types = vec![
            input_device::InputDeviceType::Touch,
            input_device::InputDeviceType::ConsumerControls,
        ];
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("input_pipeline");
        // Create fake input handler for assembly
        let (fake_handler_event_sender, _fake_handler_event_receiver) =
            futures::channel::mpsc::channel(100);
        let fake_input_handler =
            observe_fake_events_input_handler::ObserveFakeEventsInputHandler::new(
                fake_handler_event_sender,
            );
        let assembly = InputPipelineAssembly::new(metrics::MetricsLogger::default())
            .add_handler(fake_input_handler);
        let _test_input_pipeline = InputPipeline::new(
            device_types,
            assembly,
            test_node,
            metrics::MetricsLogger::default(),
        );
        fuchsia_inspect::assert_data_tree!(inspector, root: {
            input_pipeline: {
                supported_input_devices: "Touch, ConsumerControls",
                handlers_registered: 1u64,
                handlers_healthy: 1u64,
                input_devices: {}
            }
        });
    }
}
