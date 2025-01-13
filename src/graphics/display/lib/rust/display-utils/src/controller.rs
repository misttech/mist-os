// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use display_types::IMAGE_TILING_TYPE_LINEAR;

use fidl::endpoints::ClientEnd;
use fidl_fuchsia_hardware_display::{
    self as display, CoordinatorListenerRequest, LayerId as FidlLayerId,
};
use fidl_fuchsia_hardware_display_types::{self as display_types};
use fidl_fuchsia_io as fio;
use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use fuchsia_component::client::connect_to_protocol_at_path;
use fuchsia_fs::directory::{WatchEvent, Watcher};
use fuchsia_sync::RwLock;
use futures::channel::mpsc;
use futures::{future, TryStreamExt};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zx::{self as zx, HandleBased};

use crate::config::{DisplayConfig, LayerConfig};
use crate::error::{ConfigError, Error, Result};
use crate::types::{
    BufferCollectionId, BufferId, DisplayId, DisplayInfo, Event, EventId, ImageId, LayerId,
};
use crate::INVALID_EVENT_ID;

const DEV_DIR_PATH: &str = "/dev/class/display-coordinator";
const TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(2);

/// Client abstraction for the `fuchsia.hardware.display.Coordinator` protocol. Instances can be
/// safely cloned and passed across threads.
#[derive(Clone)]
pub struct Coordinator {
    inner: Arc<RwLock<CoordinatorInner>>,
}

struct CoordinatorInner {
    displays: Vec<DisplayInfo>,
    proxy: display::CoordinatorProxy,
    listener_requests: Option<display::CoordinatorListenerRequestStream>,

    // All subscribed vsync listeners and their optional ID filters.
    vsync_listeners: Vec<(mpsc::UnboundedSender<VsyncEvent>, Option<DisplayId>)>,

    // Simple counter to generate client-assigned integer identifiers.
    id_counter: u64,

    // Generate stamps for `apply_config()`.
    stamp_counter: u64,
}

/// A vsync event payload.
#[derive(Debug)]
pub struct VsyncEvent {
    /// The ID of the display that generated the vsync event.
    pub id: DisplayId,

    /// The monotonic timestamp of the vsync event.
    pub timestamp: zx::MonotonicInstant,

    /// The stamp of the latest fully applied display configuration.
    pub config: display_types::ConfigStamp,
}

impl Coordinator {
    /// Establishes a connection to the display-coordinator device and initialize a `Coordinator`
    /// instance with the initial set of available displays. The returned `Coordinator` will
    /// maintain FIDL connection to the underlying device as long as it is alive or the connection
    /// is closed by the peer.
    ///
    /// Returns an error if
    /// - No display-coordinator device is found within `TIMEOUT`.
    /// - An initial OnDisplaysChanged event is not received from the display driver within
    ///   `TIMEOUT` seconds.
    ///
    /// Current limitations:
    ///   - This function connects to the first display-coordinator device that it observes. It
    ///   currently does not support selection of a specific device if multiple display-coordinator
    ///   devices are present.
    // TODO(https://fxbug.dev/42168593): This will currently result in an error if no displays are present on
    // the system (or if one is not attached within `TIMEOUT`). It wouldn't be neceesary to rely on
    // a timeout if the display driver sent en event with no displays.
    pub async fn init() -> Result<Coordinator> {
        let path = watch_first_file(DEV_DIR_PATH)
            .on_timeout(TIMEOUT.after_now(), || Err(Error::DeviceNotFound))
            .await?;
        let path = path.to_str().ok_or(Error::DevicePathInvalid)?;
        let provider_proxy = connect_to_protocol_at_path::<display::ProviderMarker>(path)
            .map_err(Error::DeviceConnectionError)?;

        let (coordinator_proxy, coordinator_server_end) =
            fidl::endpoints::create_proxy::<display::CoordinatorMarker>();
        let (coordinator_listener_client_end, coordinator_listener_requests) =
            fidl::endpoints::create_request_stream::<display::CoordinatorListenerMarker>();

        // TODO(https://fxbug.dev/42075865): Consider supporting virtcon client
        // connections.
        let payload = display::ProviderOpenCoordinatorWithListenerForPrimaryRequest {
            coordinator: Some(coordinator_server_end),
            coordinator_listener: Some(coordinator_listener_client_end),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        let () = provider_proxy
            .open_coordinator_with_listener_for_primary(payload)
            .await?
            .map_err(zx::Status::from_raw)?;

        Self::init_with_proxy_and_listener_requests(
            coordinator_proxy,
            coordinator_listener_requests,
        )
        .await
    }

    /// Initialize a `Coordinator` instance from pre-established Coordinator and
    /// CoordinatorListener channels.
    ///
    /// Returns an error if
    /// - An initial OnDisplaysChanged event is not received from the display driver within
    ///   `TIMEOUT` seconds.
    // TODO(https://fxbug.dev/42168593): This will currently result in an error if no displays are
    // present on the system (or if one is not attached within `TIMEOUT`). It wouldn't be neceesary
    // to rely on a timeout if the display driver sent en event with no displays.
    pub async fn init_with_proxy_and_listener_requests(
        coordinator_proxy: display::CoordinatorProxy,
        mut listener_requests: display::CoordinatorListenerRequestStream,
    ) -> Result<Coordinator> {
        let displays = wait_for_initial_displays(&mut listener_requests)
            .on_timeout(TIMEOUT.after_now(), || Err(Error::NoDisplays))
            .await?
            .into_iter()
            .map(DisplayInfo)
            .collect::<Vec<_>>();
        Ok(Coordinator {
            inner: Arc::new(RwLock::new(CoordinatorInner {
                proxy: coordinator_proxy,
                listener_requests: Some(listener_requests),
                displays,
                vsync_listeners: Vec::new(),
                id_counter: 0,
                stamp_counter: 0,
            })),
        })
    }

    /// Returns a copy of the list of displays that are currently known to be present on the system.
    pub fn displays(&self) -> Vec<DisplayInfo> {
        self.inner.read().displays.clone()
    }

    /// Returns a clone of the underlying FIDL client proxy.
    ///
    /// Note: This can be helpful to prevent holding the inner RwLock when awaiting a chained FIDL
    /// call over a proxy.
    pub fn proxy(&self) -> display::CoordinatorProxy {
        self.inner.read().proxy.clone()
    }

    /// Tell the driver to enable vsync notifications and register a channel to listen to vsync events.
    pub fn add_vsync_listener(
        &self,
        id: Option<DisplayId>,
    ) -> Result<mpsc::UnboundedReceiver<VsyncEvent>> {
        self.inner.read().proxy.set_vsync_event_delivery(true)?;

        // TODO(armansito): Switch to a bounded channel instead.
        let (sender, receiver) = mpsc::unbounded::<VsyncEvent>();
        self.inner.write().vsync_listeners.push((sender, id));
        Ok(receiver)
    }

    /// Returns a Future that represents the FIDL event handling task. Once scheduled on an
    /// executor, this task will continuously handle incoming FIDL events from the display stack
    /// and the returned Future will not terminate until the FIDL channel is closed.
    ///
    /// This task can be scheduled safely on any thread.
    pub async fn handle_events(&self) -> Result<()> {
        let inner = self.inner.clone();
        let mut events = inner.write().listener_requests.take().ok_or(Error::AlreadyRequested)?;
        while let Some(msg) = events.try_next().await? {
            match msg {
                CoordinatorListenerRequest::OnDisplaysChanged {
                    added,
                    removed,
                    control_handle: _,
                } => {
                    let removed =
                        removed.into_iter().map(|id| id.into()).collect::<Vec<DisplayId>>();
                    inner.read().handle_displays_changed(added, removed);
                }
                CoordinatorListenerRequest::OnVsync {
                    display_id,
                    timestamp,
                    applied_config_stamp,
                    cookie,
                    control_handle: _,
                } => {
                    inner.write().handle_vsync(
                        display_id.into(),
                        zx::MonotonicInstant::from_nanos(timestamp),
                        applied_config_stamp,
                        cookie,
                    )?;
                }
                _ => continue,
            }
        }
        Ok(())
    }

    /// Allocates a new virtual hardware layer that is not associated with any display and has no
    /// configuration.
    pub async fn create_layer(&self) -> Result<LayerId> {
        Ok(self.proxy().create_layer().await?.map_err(zx::Status::from_raw)?.into())
    }

    /// Creates and registers a zircon event with the display driver. The returned event can be
    /// used as a fence in a display configuration.
    pub fn create_event(&self) -> Result<Event> {
        let event = zx::Event::create();
        let remote = event.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
        let id = self.inner.write().next_free_event_id()?;

        self.inner.read().proxy.import_event(zx::Event::from(remote), &id.into())?;
        Ok(Event::new(id, event))
    }

    /// Apply a display configuration. The client is expected to receive a vsync event once the
    /// configuration is successfully applied. Returns an error if the FIDL message cannot be sent.
    pub async fn apply_config(
        &self,
        configs: &[DisplayConfig],
    ) -> std::result::Result<u64, ConfigError> {
        let proxy = self.proxy();
        for config in configs {
            proxy.set_display_layers(
                &config.id.into(),
                &config.layers.iter().map(|l| l.id.into()).collect::<Vec<FidlLayerId>>(),
            )?;
            for layer in &config.layers {
                match &layer.config {
                    LayerConfig::Color { color } => {
                        let fidl_color = fidl_fuchsia_hardware_display_types::Color::from(color);
                        proxy.set_layer_color_config(&layer.id.into(), &fidl_color)?;
                    }
                    LayerConfig::Primary { image_id, image_metadata, unblock_event } => {
                        proxy.set_layer_primary_config(&layer.id.into(), &image_metadata)?;
                        proxy.set_layer_image2(
                            &layer.id.into(),
                            &(*image_id).into(),
                            &unblock_event.unwrap_or(INVALID_EVENT_ID).into(),
                        )?;
                    }
                }
            }
        }

        let (result, ops) = proxy.check_config(false).await?;
        if result != display_types::ConfigResult::Ok {
            return Err(ConfigError::invalid(result, ops));
        }

        let config_stamp = self.inner.write().next_config_stamp().unwrap();
        let payload = fidl_fuchsia_hardware_display::CoordinatorApplyConfig3Request {
            stamp: Some(fidl_fuchsia_hardware_display_types::ConfigStamp { value: config_stamp }),
            ..Default::default()
        };
        match proxy.apply_config3(payload) {
            Ok(()) => Ok(config_stamp),
            Err(err) => Err(ConfigError::from(err)),
        }
    }

    /// Get the config stamp value of the most recent applied config in
    /// `apply_config`. Returns an error if the FIDL message cannot be sent.
    pub async fn get_recent_applied_config_stamp(&self) -> std::result::Result<u64, Error> {
        let proxy = self.proxy();
        let response = proxy.get_latest_applied_config_stamp().await?;
        Ok(response.value)
    }

    /// Import a sysmem buffer collection. The returned `BufferCollectionId` can be used in future
    /// API calls to refer to the imported collection.
    pub(crate) async fn import_buffer_collection(
        &self,
        token: ClientEnd<fidl_fuchsia_sysmem2::BufferCollectionTokenMarker>,
    ) -> Result<BufferCollectionId> {
        let id = self.inner.write().next_free_collection_id()?;
        let proxy = self.proxy();

        // First import the token.
        proxy.import_buffer_collection(&id.into(), token).await?.map_err(zx::Status::from_raw)?;

        // Tell the driver to assign any device-specific constraints.
        // TODO(https://fxbug.dev/42166207): These fields are effectively unused except for `type` in the case
        // of IMAGE_TYPE_CAPTURE.
        proxy
            .set_buffer_collection_constraints(
                &id.into(),
                &display_types::ImageBufferUsage { tiling_type: IMAGE_TILING_TYPE_LINEAR },
            )
            .await?
            .map_err(zx::Status::from_raw)?;
        Ok(id)
    }

    /// Notify the display driver to release its handle on a previously imported buffer collection.
    pub(crate) fn release_buffer_collection(&self, id: BufferCollectionId) -> Result<()> {
        self.inner.read().proxy.release_buffer_collection(&id.into()).map_err(Error::from)
    }

    /// Register a sysmem buffer collection backed image to the display driver.
    pub(crate) async fn import_image(
        &self,
        collection_id: BufferCollectionId,
        image_id: ImageId,
        image_metadata: display_types::ImageMetadata,
    ) -> Result<()> {
        self.proxy()
            .import_image(
                &image_metadata,
                &BufferId::new(collection_id, 0).into(),
                &image_id.into(),
            )
            .await?
            .map_err(zx::Status::from_raw)?;
        Ok(())
    }
}

// fmt::Debug implementation to allow a `Coordinator` instance to be used with a debug format
// specifier. We use a custom implementation as not all `Coordinator` members derive fmt::Debug.
impl fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Coordinator").field("displays", &self.displays()).finish()
    }
}

impl CoordinatorInner {
    fn next_free_collection_id(&mut self) -> Result<BufferCollectionId> {
        self.id_counter = self.id_counter.checked_add(1).ok_or(Error::IdsExhausted)?;
        Ok(BufferCollectionId(self.id_counter))
    }

    fn next_free_event_id(&mut self) -> Result<EventId> {
        self.id_counter = self.id_counter.checked_add(1).ok_or(Error::IdsExhausted)?;
        Ok(EventId(self.id_counter))
    }

    fn next_config_stamp(&mut self) -> Result<u64> {
        self.stamp_counter = self.stamp_counter.checked_add(1).ok_or(Error::IdsExhausted)?;
        Ok(self.stamp_counter)
    }

    fn handle_displays_changed(&self, _added: Vec<display::Info>, _removed: Vec<DisplayId>) {
        // TODO(armansito): update the displays list and notify clients. Terminate vsync listeners
        // that are attached to a removed display.
    }

    fn handle_vsync(
        &mut self,
        display_id: DisplayId,
        timestamp: zx::MonotonicInstant,
        applied_config_stamp: display_types::ConfigStamp,
        cookie: display::VsyncAckCookie,
    ) -> Result<()> {
        self.proxy.acknowledge_vsync(cookie.value)?;

        let mut listeners_to_remove = Vec::new();
        for (pos, (sender, filter)) in self.vsync_listeners.iter().enumerate() {
            // Skip the listener if it has a filter that does not match `display_id`.
            if filter.as_ref().map_or(false, |id| *id != display_id) {
                continue;
            }
            let payload = VsyncEvent { id: display_id, timestamp, config: applied_config_stamp };
            if let Err(e) = sender.unbounded_send(payload) {
                if e.is_disconnected() {
                    listeners_to_remove.push(pos);
                } else {
                    return Err(e.into());
                }
            }
        }

        // Clean up disconnected listeners.
        listeners_to_remove.into_iter().for_each(|pos| {
            self.vsync_listeners.swap_remove(pos);
        });

        Ok(())
    }
}

// Asynchronously returns the path to the first file found under the given directory path. The
// returned future does not resolve until either an entry is found or there is an error while
// watching the directory.
async fn watch_first_file(path: &str) -> Result<PathBuf> {
    let dir = fuchsia_fs::directory::open_in_namespace(path, fio::PERM_READABLE)?;

    let mut watcher = Watcher::new(&dir).await?;
    while let Some(msg) = watcher.try_next().await? {
        match msg.event {
            WatchEvent::EXISTING | WatchEvent::ADD_FILE => {
                if msg.filename == Path::new(".") {
                    continue;
                }
                return Ok(Path::new(path).join(msg.filename));
            }
            _ => continue,
        }
    }
    Err(Error::DeviceNotFound)
}

// Waits for a single fuchsia.hardware.display.Coordinator.OnDisplaysChanged event and returns the
// reported displays. By API contract, this event will fire at least once upon initial channel
// connection if any displays are present. If no displays are present, then the returned Future
// will not resolve until a display is plugged in.
async fn wait_for_initial_displays(
    listener_requests: &mut display::CoordinatorListenerRequestStream,
) -> Result<Vec<display::Info>> {
    let mut stream = listener_requests.try_filter_map(|event| match event {
        CoordinatorListenerRequest::OnDisplaysChanged { added, removed: _, control_handle: _ } => {
            future::ok(Some(added))
        }
        _ => future::ok(None),
    });
    stream.try_next().await?.ok_or(Error::NoDisplays)
}

#[cfg(test)]
mod tests {
    use super::{Coordinator, DisplayId, VsyncEvent};
    use anyhow::{format_err, Context, Result};
    use assert_matches::assert_matches;
    use display_mocks::{create_proxy_and_mock, MockCoordinator};
    use fuchsia_async::TestExecutor;
    use futures::task::Poll;
    use futures::{pin_mut, select, FutureExt, StreamExt};
    use {
        fidl_fuchsia_hardware_display as display,
        fidl_fuchsia_hardware_display_types as display_types,
    };

    async fn init_with_proxy_and_listener_requests(
        coordinator_proxy: display::CoordinatorProxy,
        listener_requests: display::CoordinatorListenerRequestStream,
    ) -> Result<Coordinator> {
        Coordinator::init_with_proxy_and_listener_requests(coordinator_proxy, listener_requests)
            .await
            .context("failed to initialize Coordinator")
    }

    // Returns a Coordinator and a connected mock FIDL server. This function sets up the initial
    // "OnDisplaysChanged" event with the given list of `displays`, which `Coordinator` requires
    // before it can resolve its initialization Future.
    async fn init_with_displays(
        displays: &[display::Info],
    ) -> Result<(Coordinator, MockCoordinator)> {
        let (coordinator_proxy, listener_requests, mut mock) = create_proxy_and_mock()?;
        mock.assign_displays(displays.to_vec())?;

        Ok((
            init_with_proxy_and_listener_requests(coordinator_proxy, listener_requests).await?,
            mock,
        ))
    }

    #[fuchsia::test]
    async fn test_init_fails_with_no_device_dir() {
        let result = Coordinator::init().await;
        assert_matches!(result, Err(_));
    }

    #[fuchsia::test]
    async fn test_init_with_no_displays() -> Result<()> {
        let (coordinator_proxy, listener_requests, mut mock) = create_proxy_and_mock()?;
        mock.assign_displays([].to_vec())?;

        let coordinator =
            init_with_proxy_and_listener_requests(coordinator_proxy, listener_requests).await?;
        assert!(coordinator.displays().is_empty());

        Ok(())
    }

    // TODO(https://fxbug.dev/42075852): We should have an automated test verifying that
    // the service provided by driver framework can be opened correctly.

    #[fuchsia::test]
    async fn test_init_with_displays() -> Result<()> {
        let displays = [
            display::Info {
                id: display_types::DisplayId { value: 1 },
                modes: Vec::new(),
                pixel_format: Vec::new(),
                manufacturer_name: "Foo".to_string(),
                monitor_name: "what".to_string(),
                monitor_serial: "".to_string(),
                horizontal_size_mm: 0,
                vertical_size_mm: 0,
                using_fallback_size: false,
            },
            display::Info {
                id: display_types::DisplayId { value: 2 },
                modes: Vec::new(),
                pixel_format: Vec::new(),
                manufacturer_name: "Bar".to_string(),
                monitor_name: "who".to_string(),
                monitor_serial: "".to_string(),
                horizontal_size_mm: 0,
                vertical_size_mm: 0,
                using_fallback_size: false,
            },
        ]
        .to_vec();
        let (coordinator_proxy, listener_requests, mut mock) = create_proxy_and_mock()?;
        mock.assign_displays(displays.clone())?;

        let coordinator =
            init_with_proxy_and_listener_requests(coordinator_proxy, listener_requests).await?;
        assert_eq!(coordinator.displays().len(), 2);
        assert_eq!(coordinator.displays()[0].0, displays[0]);
        assert_eq!(coordinator.displays()[1].0, displays[1]);

        Ok(())
    }

    #[test]
    fn test_vsync_listener_single() -> Result<()> {
        // Drive an executor directly for this test to avoid having to rely on timeouts for cases
        // in which no events are received.
        let mut executor = TestExecutor::new();
        let (coordinator, mock) = executor.run_singlethreaded(init_with_displays(&[]))?;
        let mut vsync = coordinator.add_vsync_listener(None)?;

        const ID: DisplayId = DisplayId(1);
        const STAMP: display_types::ConfigStamp = display_types::ConfigStamp { value: 1 };
        let event_handlers = async {
            select! {
                event = vsync.next() => event.ok_or(format_err!("did not receive vsync event")),
                result = coordinator.handle_events().fuse() => {
                    result.context("FIDL event handler failed")?;
                    Err(format_err!("FIDL event handler completed before client vsync event"))
                },
            }
        };
        pin_mut!(event_handlers);

        // Send a single event.
        mock.emit_vsync_event(ID.0, STAMP)?;
        let vsync_event = executor.run_until_stalled(&mut event_handlers);
        assert_matches!(
            vsync_event,
            Poll::Ready(Ok(VsyncEvent { id: ID, timestamp: _, config: STAMP }))
        );

        Ok(())
    }

    #[test]
    fn test_vsync_listener_multiple() -> Result<()> {
        // Drive an executor directly for this test to avoid having to rely on timeouts for cases
        // in which no events are received.
        let mut executor = TestExecutor::new();
        let (coordinator, mock) = executor.run_singlethreaded(init_with_displays(&[]))?;
        let mut vsync = coordinator.add_vsync_listener(None)?;

        let fidl_server = coordinator.handle_events().fuse();
        pin_mut!(fidl_server);

        const ID1: DisplayId = DisplayId(1);
        const ID2: DisplayId = DisplayId(2);
        const STAMP: display_types::ConfigStamp = display_types::ConfigStamp { value: 1 };

        // Queue multiple events.
        mock.emit_vsync_event(ID1.0, STAMP)?;
        mock.emit_vsync_event(ID2.0, STAMP)?;
        mock.emit_vsync_event(ID1.0, STAMP)?;

        // Process the FIDL events. The FIDL server Future should not complete as it runs
        // indefinitely.
        let fidl_server_result = executor.run_until_stalled(&mut fidl_server);
        assert_matches!(fidl_server_result, Poll::Pending);

        // Process the vsync listener.
        let vsync_event = executor.run_until_stalled(&mut Box::pin(async { vsync.next().await }));
        assert_matches!(
            vsync_event,
            Poll::Ready(Some(VsyncEvent { id: ID1, timestamp: _, config: STAMP }))
        );

        let vsync_event = executor.run_until_stalled(&mut Box::pin(async { vsync.next().await }));
        assert_matches!(
            vsync_event,
            Poll::Ready(Some(VsyncEvent { id: ID2, timestamp: _, config: STAMP }))
        );

        let vsync_event = executor.run_until_stalled(&mut Box::pin(async { vsync.next().await }));
        assert_matches!(
            vsync_event,
            Poll::Ready(Some(VsyncEvent { id: ID1, timestamp: _, config: STAMP }))
        );

        Ok(())
    }

    #[test]
    fn test_vsync_listener_display_id_filter() -> Result<()> {
        // Drive an executor directly for this test to avoid having to rely on timeouts for cases
        // in which no events are received.
        let mut executor = TestExecutor::new();
        let (coordinator, mock) = executor.run_singlethreaded(init_with_displays(&[]))?;

        const ID1: DisplayId = DisplayId(1);
        const ID2: DisplayId = DisplayId(2);
        const STAMP: display_types::ConfigStamp = display_types::ConfigStamp { value: 1 };

        // Listen to events from ID2.
        let mut vsync = coordinator.add_vsync_listener(Some(ID2))?;
        let event_handlers = async {
            select! {
                event = vsync.next() => event.ok_or(format_err!("did not receive vsync event")),
                result = coordinator.handle_events().fuse() => {
                    result.context("FIDL event handler failed")?;
                    Err(format_err!("FIDL event handler completed before client vsync event"))
                },
            }
        };
        pin_mut!(event_handlers);

        // Event from ID1 should get filtered out and the client should not receive any events.
        mock.emit_vsync_event(ID1.0, STAMP)?;
        let vsync_event = executor.run_until_stalled(&mut event_handlers);
        assert_matches!(vsync_event, Poll::Pending);

        // Event from ID2 should be received.
        mock.emit_vsync_event(ID2.0, STAMP)?;
        let vsync_event = executor.run_until_stalled(&mut event_handlers);
        assert_matches!(
            vsync_event,
            Poll::Ready(Ok(VsyncEvent { id: ID2, timestamp: _, config: STAMP }))
        );

        Ok(())
    }
}
