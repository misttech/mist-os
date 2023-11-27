// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains for creating and serving a `Flatland` view using a `Framebuffer`.
//!
//! A lot of the code in this file is temporary to enable developers to see the contents of a
//! `Framebuffer` in the workstation UI (e.g., using `ffx session add`).
//!
//! To display the `Framebuffer` as its view, a component must add the `framebuffer` feature to its
//! `.cml`.

use crate::task::Kernel;
use anyhow::anyhow;
use fidl::{
    endpoints::{create_proxy, create_request_stream},
    HandleBased,
};
use fidl_fuchsia_element as felement;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_sysmem as fsysmem;
use fidl_fuchsia_ui_app as fuiapp;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_views as fuiviews;
use flatland_frame_scheduling_lib::{
    PresentationInfo, PresentedInfo, SchedulingLib, ThroughputScheduler,
};
use fuchsia_async as fasync;
use fuchsia_component::{
    client::{connect_to_protocol_at_dir_root, connect_to_protocol_sync},
    server::ServiceFs,
};
use fuchsia_framebuffer::{sysmem::BufferCollectionAllocator, FrameUsage};
use fuchsia_scenic::{flatland::ViewCreationTokenPair, BufferCollectionTokenPair};
use fuchsia_zircon as zx;
use futures::{StreamExt, TryStreamExt};
use lifecycle::AtomicU64Counter;
use starnix_lock::Mutex;
use std::{
    ops::DerefMut,
    sync::{mpsc::channel, Arc},
};

use crate::logging::log_warn;
use starnix_uapi::{errno, errors::Errno};

/// The offset at which the framebuffer will be placed.
pub const TRANSLATION_X: i32 = 0;

/// The Flatland identifier for the transform associated with the framebuffer.
const ROOT_TRANSFORM_ID: fuicomposition::TransformId = fuicomposition::TransformId { value: 1 };

/// The Flatland identifier for the framebuffer image.
const FB_IMAGE_ID: fuicomposition::ContentId = fuicomposition::ContentId { value: 1 };

/// The protocols that are exposed by the framebuffer server.
enum ExposedProtocols {
    ViewProvider(fuiapp::ViewProviderRequestStream),
}

/// The Scene states that `FramebufferServer` may serve.
pub enum SceneState {
    Fb,
    Viewport,
}

/// A `FramebufferServer` contains initialized proxies to Flatland, as well as a buffer collection
/// that is registered with Flatland.
pub struct FramebufferServer {
    /// The Flatland proxy associated with this server.
    flatland: fuicomposition::FlatlandSynchronousProxy,

    /// The buffer collection that is registered with Flatland.
    collection: fsysmem::BufferCollectionInfo2,

    /// The width of the display and framebuffer image.
    image_width: u32,

    /// The height of the display and framebuffer image.
    image_height: u32,

    /// Keeps track if this class is serving FB or a Viewport.
    scene_state: Arc<Mutex<SceneState>>,

    /// Keeps track of the Flatland viewport ID.
    viewport_id: AtomicU64Counter,
}

impl FramebufferServer {
    /// Returns a `FramebufferServer` that has created a scene and registered a buffer with
    /// Flatland.
    pub fn new(width: u32, height: u32) -> Result<Self, Errno> {
        let allocator = connect_to_protocol_sync::<fuicomposition::AllocatorMarker>()
            .map_err(|_| errno!(ENOENT))?;
        let flatland = connect_to_protocol_sync::<fuicomposition::FlatlandMarker>()
            .map_err(|_| errno!(ENOENT))?;
        flatland.set_debug_name("StarnixFrameBufferServer").map_err(|_| errno!(EINVAL))?;

        let collection =
            init_fb_scene(&flatland, &allocator, width, height).map_err(|_| errno!(EINVAL))?;

        Ok(Self {
            flatland,
            collection,
            image_width: width,
            image_height: height,
            scene_state: Arc::new(Mutex::new(SceneState::Fb)),
            viewport_id: (FB_IMAGE_ID.value + 1).into(),
        })
    }

    /// Returns a clone of the VMO that is shared with Flatland.
    pub fn get_vmo(&self) -> Result<zx::Vmo, Errno> {
        self.collection.buffers[0]
            .vmo
            .as_ref()
            .ok_or_else(|| errno!(EINVAL))?
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(|_| errno!(EINVAL))
    }
}

/// Initializes the flatland scene, and returns the associated buffer collection.
///
/// SAFETY: This function `.expect`'s a lot, because it isn't meant to be used in the long time and
/// most of the failures would be unexpected and unrecoverable.
fn init_fb_scene(
    flatland: &fuicomposition::FlatlandSynchronousProxy,
    allocator: &fuicomposition::AllocatorSynchronousProxy,
    width: u32,
    height: u32,
) -> Result<fsysmem::BufferCollectionInfo2, anyhow::Error> {
    flatland
        .create_transform(&ROOT_TRANSFORM_ID)
        .map_err(|_| anyhow!("error creating transform"))?;
    flatland
        .set_root_transform(&ROOT_TRANSFORM_ID)
        .map_err(|_| anyhow!("error setting root transform"))?;

    let (collection_sender, collection_receiver) = channel();
    let (allocation_sender, allocation_receiver) = channel();
    // This thread is spawned to deal with the mix of asynchronous and synchronous proxies.
    // In particular, we want to keep Framebuffer creation synchronous, while still making use of
    // BufferCollectionAllocator (which exposes an async api).
    //
    // The spawned thread will execute the futures and send results back to this thread via a
    // channel.
    std::thread::Builder::new()
        .name("kthread-fb-alloc".to_string())
        .spawn(move || -> Result<(), anyhow::Error> {
            let mut executor = fasync::LocalExecutor::new();

            let mut buffer_allocator = BufferCollectionAllocator::new(
                width,
                height,
                fidl_fuchsia_sysmem::PixelFormatType::R8G8B8A8,
                FrameUsage::Cpu,
                1,
            )?;
            buffer_allocator.set_name(100, "Starnix ViewProvider")?;

            let sysmem_buffer_collection_token =
                executor.run_singlethreaded(buffer_allocator.duplicate_token())?;
            // Notify the async code that the sysmem buffer collection token is available.
            collection_sender
                .send(sysmem_buffer_collection_token)
                .expect("Failed to send collection");

            let allocation =
                executor.run_singlethreaded(buffer_allocator.allocate_buffers(true))?;
            // Notify the async code that the buffer allocation completed.
            allocation_sender.send(allocation).expect("Failed to send allocation");

            Ok(())
        })
        .expect("able to create threads");

    // Wait for the async code to generate the buffer collection token.
    let sysmem_buffer_collection_token = collection_receiver
        .recv()
        .map_err(|_| anyhow!("Error receiving buffer collection token"))?;

    let buffer_tokens = BufferCollectionTokenPair::new();
    let args = fuicomposition::RegisterBufferCollectionArgs {
        export_token: Some(buffer_tokens.export_token),
        buffer_collection_token: Some(sysmem_buffer_collection_token),
        ..Default::default()
    };

    allocator
        .register_buffer_collection(args, zx::Time::INFINITE)
        .map_err(|_| anyhow!("FIDL error registering buffer collection"))?
        .map_err(|_| anyhow!("Error registering buffer collection"))?;

    // Now that the buffer collection is registered, wait for the buffer allocation to happen.
    let allocation =
        allocation_receiver.recv().map_err(|_| anyhow!("Error receiving buffer allocation"))?;

    let image_props = fuicomposition::ImageProperties {
        size: Some(fmath::SizeU { width, height }),
        ..Default::default()
    };
    flatland
        .create_image(&FB_IMAGE_ID, buffer_tokens.import_token, 0, &image_props)
        .map_err(|_| anyhow!("FIDL error creating image"))?;
    flatland
        .set_image_destination_size(&FB_IMAGE_ID, &fmath::SizeU { width, height })
        .expect("FIDL error resizing image");
    flatland
        .set_content(&ROOT_TRANSFORM_ID, &FB_IMAGE_ID)
        .map_err(|_| anyhow!("error setting content"))?;
    flatland
        .set_translation(&ROOT_TRANSFORM_ID, &fmath::Vec_ { x: TRANSLATION_X, y: 0 })
        .map_err(|_| anyhow!("error setting translation"))?;

    Ok(allocation)
}

/// Initializes a flatland scene where only the child view is presented through
/// `ViewportCreationToken`.
pub fn init_viewport_scene(
    server: Arc<FramebufferServer>,
    viewport_token: fuiviews::ViewportCreationToken,
) {
    let mut scene_state = server.scene_state.lock();
    let scene_state = scene_state.deref_mut();
    // This handles multiple calls to init_viewport_scene and cleans fb resources.
    // TODO(b/282895335): Break the present loop if already started. Note that we still need to
    // present once, taking into account the present credits
    match scene_state {
        SceneState::Fb => {
            server.flatland.release_image(&FB_IMAGE_ID).expect("failed to release image");
        }
        SceneState::Viewport => {
            // TODO(b/282895335): Release viewport depends on a concurrent Present to return.
            // Refactor the present loop to handle.
        }
    }

    let (_, child_view_watcher_request) = create_proxy::<fuicomposition::ChildViewWatcherMarker>()
        .expect("failed to create child view watcher channel");
    let viewport_properties = fuicomposition::ViewportProperties {
        logical_size: Some(fmath::SizeU { width: server.image_width, height: server.image_height }),
        ..Default::default()
    };
    let new_viewport = fuicomposition::ContentId { value: server.viewport_id.next() };
    server
        .flatland
        .create_viewport(
            &new_viewport,
            viewport_token,
            &viewport_properties,
            child_view_watcher_request,
        )
        .expect("failed to create child viewport");
    server.flatland.set_content(&ROOT_TRANSFORM_ID, &new_viewport).expect("error setting content");

    *scene_state = SceneState::Viewport;
}

/// Spawns a thread to serve a `ViewProvider` in `outgoing_dir`.
///
/// SAFETY: This function `.expect`'s a lot, because it isn't meant to be used in the long time and
/// most of the failures would be unexpected and unrecoverable.
pub fn spawn_view_provider(
    server: Arc<FramebufferServer>,
    view_bound_protocols: fuicomposition::ViewBoundProtocols,
    view_identity: fuiviews::ViewIdentityOnCreation,
    outgoing_dir: fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>,
) {
    std::thread::Builder::new().name("kthread-view-provider".to_string()).spawn(|| {
        let mut executor = fasync::LocalExecutor::new();
        let mut view_bound_protocols = Some(view_bound_protocols);
        let mut view_identity = Some(view_identity);
        executor.run_singlethreaded(async move {
            let mut service_fs = ServiceFs::new_local();
            service_fs.dir("svc").add_fidl_service(ExposedProtocols::ViewProvider);
            service_fs.serve_connection(outgoing_dir).expect("");

            while let Some(ExposedProtocols::ViewProvider(mut request_stream)) =
                service_fs.next().await
            {
                while let Ok(Some(event)) = request_stream.try_next().await {
                    match event {
                        fuiapp::ViewProviderRequest::CreateView2 { args, control_handle: _ } => {
                            let view_creation_token = args.view_creation_token.unwrap();
                            // We don't actually care about the parent viewport at the moment, because we don't resize.
                            let (_parent_viewport_watcher, parent_viewport_watcher_request) =
                                create_proxy::<fuicomposition::ParentViewportWatcherMarker>()
                                    .expect("failed to create ParentViewportWatcherProxy");
                            server
                                .flatland
                                .create_view2(
                                     view_creation_token,
                                     view_identity.take().expect("cannot create view because view identity has been consumed"),
                                    view_bound_protocols.take().expect("cannot create view because view bound protocols have been consumed"),
                                    parent_viewport_watcher_request,
                                )
                                .expect("FIDL error");

                            // Now that the view has been created, start presenting.
                            start_presenting(server.clone());
                        }
                        r => {
                            log_warn!("Got unexpected view provider request: {:?}", r);
                        }
                    }
                }
            }
        });
    }).expect("able to create threads");
}

pub fn present_view(
    kernel: &Arc<Kernel>,
    server: Arc<FramebufferServer>,
    view_bound_protocols: fuicomposition::ViewBoundProtocols,
    view_identity: fuiviews::ViewIdentityOnCreation,
    incoming_svc_dir: fidl_fuchsia_io::DirectorySynchronousProxy,
) {
    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        let mut view_bound_protocols = Some(view_bound_protocols);
        let mut view_identity = Some(view_identity);
        let mut maybe_view_controller_proxy = None;
        executor.run_singlethreaded(async move {
            let link_token_pair =
                ViewCreationTokenPair::new().expect("failed to create ViewCreationTokenPair");
            // We don't actually care about the parent viewport at the moment, because we don't resize.
            let (_parent_viewport_watcher, parent_viewport_watcher_request) =
                create_proxy::<fuicomposition::ParentViewportWatcherMarker>()
                    .expect("failed to create ParentViewportWatcherProxy");
            server
                .flatland
                .create_view2(
                    link_token_pair.view_creation_token,
                    view_identity
                        .take()
                        .expect("cannot create view because view identity has been consumed"),
                    view_bound_protocols.take().expect(
                        "cannot create view because view bound protocols have been consumed",
                    ),
                    parent_viewport_watcher_request,
                )
                .expect("FIDL error");

            // Now that the view has been created, start presenting.
            start_presenting(server.clone());

            let graphical_presenter = connect_to_protocol_at_dir_root::<
                felement::GraphicalPresenterMarker,
            >(&incoming_svc_dir)
            .map_err(|_| errno!(ENOENT))
            .expect("Failed to connect to GraphicalPresenter");

            let (view_controller_proxy, view_controller_server_end) =
                fidl::endpoints::create_proxy::<felement::ViewControllerMarker>()
                    .expect("failed to create ViewControllerProxy");
            let _ = maybe_view_controller_proxy.insert(view_controller_proxy);

            let view_spec = felement::ViewSpec {
                annotations: None,
                viewport_creation_token: Some(link_token_pair.viewport_creation_token),
                ..Default::default()
            };

            // TODO: b/307790211 - Service annotation controller stream.
            let (annotation_controller_client_end, _annotation_controller_stream) =
                create_request_stream::<felement::AnnotationControllerMarker>().unwrap();

            graphical_presenter
                .present_view(
                    view_spec,
                    Some(annotation_controller_client_end),
                    Some(view_controller_server_end),
                )
                .await
                .expect("failed to present view")
                .unwrap_or_else(|e| println!("{:?}", e));
        });
    });
}

/// Starts a flatland presentation loop, using the flatland proxy in `server`.
fn start_presenting(server: Arc<FramebufferServer>) {
    fasync::Task::local(async move {
        let sched_lib = ThroughputScheduler::new();
        // Request an initial presentation.
        sched_lib.request_present();

        loop {
            let present_parameters = sched_lib.wait_to_update().await;
            sched_lib.request_present();
            server
                .flatland
                .present(fuicomposition::PresentArgs {
                    requested_presentation_time: Some(
                        present_parameters.requested_presentation_time.into_nanos(),
                    ),
                    acquire_fences: None,
                    release_fences: None,
                    unsquashable: Some(present_parameters.unsquashable),
                    ..Default::default()
                })
                .unwrap_or(());

            // Wait for events from flatland. If the event is `OnFramePresented` we notify the
            // scheduler and then wait for a `OnNextFrameBegin` before continuing.
            while match server.flatland.wait_for_event(zx::Time::INFINITE) {
                Ok(event) => match event {
                    fuicomposition::FlatlandEvent::OnNextFrameBegin { values } => {
                        let fuicomposition::OnNextFrameBeginValues {
                            additional_present_credits,
                            future_presentation_infos,
                            ..
                        } = values;
                        let infos = future_presentation_infos
                            .unwrap()
                            .iter()
                            .map(|x| PresentationInfo {
                                latch_point: zx::Time::from_nanos(x.latch_point.unwrap()),
                                presentation_time: zx::Time::from_nanos(
                                    x.presentation_time.unwrap(),
                                ),
                            })
                            .collect();
                        sched_lib.on_next_frame_begin(additional_present_credits.unwrap(), infos);
                        false
                    }
                    fuicomposition::FlatlandEvent::OnFramePresented { frame_presented_info } => {
                        let presented_infos = frame_presented_info
                            .presentation_infos
                            .iter()
                            .map(|info| PresentedInfo {
                                present_received_time: zx::Time::from_nanos(
                                    info.present_received_time.unwrap(),
                                ),
                                actual_latch_point: zx::Time::from_nanos(
                                    info.latched_time.unwrap(),
                                ),
                            })
                            .collect();

                        sched_lib.on_frame_presented(
                            zx::Time::from_nanos(frame_presented_info.actual_presentation_time),
                            presented_infos,
                        );
                        true
                    }
                    fuicomposition::FlatlandEvent::OnError { .. } => false,
                },
                Err(_) => false,
            } {}
        }
    })
    .detach();
}
