// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::color_transform_manager::ColorTransformManager;
use ::input_pipeline::input_device::InputDeviceType;
#[cfg(fuchsia_api_level_at_least = "HEAD")]
use ::input_pipeline::interaction_state_handler::InteractionStateHandler;
use ::input_pipeline::light_sensor::Configuration as LightSensorConfiguration;
use anyhow::{Context, Error};
use fidl_fuchsia_accessibility::{ColorTransformHandlerMarker, ColorTransformMarker};
use fidl_fuchsia_element::{
    GraphicalPresenterRequest, GraphicalPresenterRequestStream, PresentViewError, ViewSpec,
};
use fidl_fuchsia_input_injection::InputDeviceRegistryRequestStream;
use fidl_fuchsia_input_interaction::NotifierRequestStream;
use fidl_fuchsia_input_interaction_observation::AggregatorRequestStream;
use fidl_fuchsia_lightsensor::SensorRequestStream as LightSensorRequestStream;
use fidl_fuchsia_recovery_policy::DeviceRequestStream as FactoryResetDeviceRequestStream;
use fidl_fuchsia_recovery_ui::FactoryResetCountdownRequestStream;
use fidl_fuchsia_session_scene::{
    ManagerRequest as SceneManagerRequest, ManagerRequestStream as SceneManagerRequestStream,
    PresentRootViewError,
};
use fidl_fuchsia_ui_brightness::{
    ColorAdjustmentHandlerRequestStream, ColorAdjustmentRequestStream,
};
use fidl_fuchsia_ui_focus::FocusChainProviderRequestStream;
use fidl_fuchsia_ui_policy::{
    DeviceListenerRegistryRequestStream as MediaButtonsListenerRegistryRequestStream,
    DisplayBacklightRequestStream,
};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::lock::Mutex;
use futures::{StreamExt, TryStreamExt};
use log::{error, info, warn};
use scene_management::{SceneManager, SceneManagerTrait, ViewingDistance};
use scene_manager_structured_config::Config;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use {
    fidl_fuchsia_accessibility_scene as a11y_view, fidl_fuchsia_ui_composition as flatland,
    fidl_fuchsia_ui_composition_internal as fcomp, fidl_fuchsia_ui_display_color as color,
    fidl_fuchsia_ui_display_singleton as singleton_display, fuchsia_async as fasync,
    fuchsia_inspect as inspect,
};

#[cfg(fuchsia_api_level_at_least = "HEAD")]
mod color_transform_manager;
mod factory_reset_countdown_server;
mod factory_reset_device_server;
mod input_device_registry_server;
mod input_pipeline;
mod light_sensor_server;
mod media_buttons_listener_registry_server;

enum ExposedServices {
    ColorAdjustment(ColorAdjustmentRequestStream),
    ColorAdjustmentHandler(ColorAdjustmentHandlerRequestStream),
    MediaButtonsListenerRegistry(MediaButtonsListenerRegistryRequestStream),
    DisplayBacklight(DisplayBacklightRequestStream),
    FactoryResetCountdown(FactoryResetCountdownRequestStream),
    FactoryReset(FactoryResetDeviceRequestStream),
    FocusChainProvider(FocusChainProviderRequestStream),
    GraphicalPresenter(GraphicalPresenterRequestStream),
    InputDeviceRegistry(InputDeviceRegistryRequestStream),
    LightSensor(LightSensorRequestStream),
    SceneManager(SceneManagerRequestStream),
    UserInteractionObservation(AggregatorRequestStream),
    UserInteraction(NotifierRequestStream),
}

#[fuchsia::main(logging_tags = [ "scene_manager" ])]
async fn main() -> Result<(), Error> {
    let result = inner_main().await;
    if let Err(e) = result {
        error!("Uncaught error in main(): {}", e);
        return Err(e);
    }
    Ok(())
}

const LIGHT_SENSOR_CONFIGURATION: &'static str = "/sensor-config/config.json";

// TODO(https://fxbug.dev/42170765): Ideally we wouldn't need to have separate inner_main() and main()
// functions in order to catch and log top-level errors.  Instead, the #[fuchsia::main] macro
// could catch and log the error.
async fn inner_main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();

    // Create an inspector that's large enough to store 10 seconds of touchpad
    // events.
    // * Empirically, when all events have two fingers, the total inspect data
    //   size is about 260 KB.
    // * Use a slightly larger value here to allow some headroom. E.g. perhaps
    //   some events have a third finger.
    let inspector = inspect::component::init_inspector_with_size(300 * 1024);
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    // Report data on the size of the inspect VMO, and the number of allocation
    // failures encountered. (Allocation failures can lead to missing data.)
    inspect::component::serve_inspect_stats();

    // Initialize tracing.
    //
    // This is done once by the process, rather than making the libraries
    // linked into the component (e.g. input pipeline) initialize tracing.
    //
    // Initializing at the process-level more closely models how a trace
    // provider (e.g. scene_manager) interacts with the trace manager.
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    // Do not reorder the services below.
    fs.dir("svc")
        .add_fidl_service(ExposedServices::ColorAdjustmentHandler)
        .add_fidl_service(ExposedServices::ColorAdjustment)
        .add_fidl_service(ExposedServices::MediaButtonsListenerRegistry)
        .add_fidl_service(ExposedServices::DisplayBacklight)
        .add_fidl_service(ExposedServices::FactoryResetCountdown)
        .add_fidl_service(ExposedServices::FactoryReset)
        .add_fidl_service(ExposedServices::FocusChainProvider)
        .add_fidl_service(ExposedServices::GraphicalPresenter)
        .add_fidl_service(ExposedServices::InputDeviceRegistry)
        .add_fidl_service(ExposedServices::SceneManager)
        .add_fidl_service(ExposedServices::UserInteractionObservation)
        .add_fidl_service(ExposedServices::UserInteraction);

    let light_sensor_configuration: Option<LightSensorConfiguration> =
        match File::open(LIGHT_SENSOR_CONFIGURATION) {
            Ok(mut file) => {
                let mut contents = String::new();
                let _: usize =
                    file.read_to_string(&mut contents).context("reading configuration")?;
                Some(serde_json::from_str(&contents).context("parsing configuration")?)
            }
            // Not found signifies that no configuration is supplied for the light sensor, and so it
            // should be configured off.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).context("opening light sensor config");
            }
        };
    let (light_sensor_server, light_sensor_request_stream_receiver) =
        if light_sensor_configuration.is_some() {
            let (light_sensor_server, light_sensor_request_stream_receiver) =
                light_sensor_server::make_server_and_receiver();
            (Some(light_sensor_server), Some(light_sensor_request_stream_receiver))
        } else {
            (None, None)
        };

    let (input_device_registry_server, input_device_registry_request_stream_receiver) =
        input_device_registry_server::make_server_and_receiver();

    let (
        media_buttons_listener_registry_server,
        media_buttons_listener_registry_request_stream_receiver,
    ) = media_buttons_listener_registry_server::make_server_and_receiver();

    let (factory_reset_countdown_server, factory_reset_countdown_request_stream_receiver) =
        factory_reset_countdown_server::make_server_and_receiver();

    let (factory_reset_device_server, factory_reset_device_request_stream_receiver) =
        factory_reset_device_server::make_server_and_receiver();

    // This call should normally never fail. The ICU data loader must be kept alive to ensure
    // Unicode data is kept in memory.
    let icu_data_loader = icu_data::Loader::new().unwrap();

    let ownership_proxy = connect_to_protocol::<fcomp::DisplayOwnershipMarker>()?;
    let display_ownership =
        ownership_proxy.get_event().await.expect("Failed to get display ownership.");
    info!("Instantiating SceneManager");

    let Config {
        supported_input_devices,
        display_rotation,
        display_pixel_density,
        viewing_distance,
        #[cfg(fuchsia_api_level_at_least = "HEAD")]
        idle_threshold_ms,
        #[cfg(fuchsia_api_level_at_least = "HEAD")]
        suspend_enabled,
        #[cfg(fuchsia_api_level_at_least = "HEAD")]
        attach_a11y_view,
        ..
    } = Config::take_from_startup_handle();

    let display_pixel_density = match display_pixel_density.trim().parse::<f32>() {
        Ok(density) => {
            if density < 0.0 {
                None
            } else {
                Some(density)
            }
        }
        Err(_) => {
            warn!("Failed to parse display_pixel_density value from structured config - expected a decimal, but got: {display_pixel_density}. Falling back to default.");
            None
        }
    };

    let viewing_distance = match viewing_distance.to_lowercase().trim() {
        "handheld" => Some(ViewingDistance::Handheld),
        "close" => Some(ViewingDistance::Close),
        "near" => Some(ViewingDistance::Near),
        "midrange" => Some(ViewingDistance::Midrange),
        "far" => Some(ViewingDistance::Far),
        _ => {
            warn!("No viewing_distance config value provided, falling back to default.");
            None
        }
    };

    let flatland_display = connect_to_protocol::<flatland::FlatlandDisplayMarker>()?;
    let singleton_display_info = connect_to_protocol::<singleton_display::InfoMarker>()?;
    let root_flatland = connect_to_protocol::<flatland::FlatlandMarker>()?;
    let pointerinjector_flatland = connect_to_protocol::<flatland::FlatlandMarker>()?;
    let scene_flatland = connect_to_protocol::<flatland::FlatlandMarker>()?;
    let a11y_view_provider = if attach_a11y_view {
        Some(connect_to_protocol::<a11y_view::ProviderMarker>()?)
    } else {
        None
    };
    let scene_manager: Arc<Mutex<dyn SceneManagerTrait>> = Arc::new(Mutex::new(
        SceneManager::new(
            flatland_display,
            singleton_display_info,
            root_flatland,
            pointerinjector_flatland,
            scene_flatland,
            a11y_view_provider,
            display_rotation,
            display_pixel_density,
            viewing_distance,
        )
        .await?,
    ));

    let (focus_chain_publisher, focus_chain_stream_handler) =
        focus_chain_provider::make_publisher_and_stream_handler();

    // Create a node under root to hang all input pipeline inspect data off of.
    let inspect_node = inspector.root().create_child("input_pipeline");

    // Create InteractionStateHandler.
    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    let interaction_state_handler = InteractionStateHandler::new(
        zx::MonotonicDuration::from_millis(idle_threshold_ms as i64),
        &inspect_node,
        suspend_enabled,
    )
    .await;

    // Start input pipeline.
    let has_light_sensor_configuration = light_sensor_configuration.is_some();
    if let Ok(input_pipeline) = input_pipeline::handle_input(
        scene_manager.clone(),
        input_device_registry_request_stream_receiver,
        light_sensor_request_stream_receiver,
        media_buttons_listener_registry_request_stream_receiver,
        factory_reset_countdown_request_stream_receiver,
        factory_reset_device_request_stream_receiver,
        icu_data_loader,
        inspect_node,
        display_ownership,
        focus_chain_publisher,
        supported_input_devices,
        light_sensor_configuration,
    )
    .await
    {
        if input_pipeline.input_device_types().contains(&InputDeviceType::LightSensor)
            && has_light_sensor_configuration
        {
            fs.dir("svc").add_fidl_service(ExposedServices::LightSensor);
        }
        fasync::Task::local(input_pipeline.handle_input_events()).detach();
    };

    // Create and register a ColorTransformManager.
    let color_converter = connect_to_protocol::<color::ConverterMarker>()?;
    let color_transform_manager =
        ColorTransformManager::new(color_converter, Arc::clone(&scene_manager));

    let (color_transform_handler_client, color_transform_handler_server) =
        fidl::endpoints::create_request_stream::<ColorTransformHandlerMarker>();
    match connect_to_protocol::<ColorTransformMarker>() {
        Err(e) => {
            error!("Failed to connect to fuchsia.accessibility.color_transform: {:?}", e);
        }
        Ok(proxy) => match proxy.register_color_transform_handler(color_transform_handler_client) {
            Err(e) => {
                error!("Failed to call RegisterColorTransformHandler: {:?}", e);
            }
            Ok(()) => {
                ColorTransformManager::handle_color_transform_request_stream(
                    Arc::clone(&color_transform_manager),
                    color_transform_handler_server,
                );
            }
        },
    }

    fs.take_and_serve_directory_handle()?;

    // Concurrency note: spawn a local task in the match branch if the protocol must serve more
    // than a single client at a time.
    while let Some(service_request) = fs.next().await {
        match service_request {
            ExposedServices::ColorAdjustmentHandler(request_stream) => {
                ColorTransformManager::handle_color_adjustment_handler_request_stream(
                    Arc::clone(&color_transform_manager),
                    request_stream,
                );
            }
            ExposedServices::ColorAdjustment(request_stream) => {
                ColorTransformManager::handle_color_adjustment_request_stream(
                    Arc::clone(&color_transform_manager),
                    request_stream,
                );
            }
            ExposedServices::DisplayBacklight(request_stream) => {
                ColorTransformManager::handle_display_backlight_request_stream(
                    Arc::clone(&color_transform_manager),
                    request_stream,
                );
            }
            ExposedServices::FocusChainProvider(request_stream) => {
                focus_chain_stream_handler.handle_request_stream(request_stream).detach();
            }
            ExposedServices::SceneManager(request_stream) => {
                fasync::Task::local(handle_scene_manager_request_stream(
                    request_stream,
                    Arc::clone(&scene_manager),
                ))
                .detach();
            }
            ExposedServices::InputDeviceRegistry(request_stream) => {
                match &input_device_registry_server.handle_request(request_stream).await {
                    Ok(()) => (),
                    Err(e) => {
                        // If `handle_request()` returns `Err`, then the `unbounded_send()` call
                        // from `handle_request()` failed with either:
                        // * `TrySendError::SendErrorKind::Full`, or
                        // * `TrySendError::SendErrorKind::Disconnected`.
                        //
                        // These are unexpected, because:
                        // * `Full` can't happen, because `InputDeviceRegistryServer`
                        //   uses an `UnboundedSender`.
                        // * `Disconnected` is highly unlikely, because the corresponding
                        //   `UnboundedReceiver` lives in `main::input_fut`, and `input_fut`'s
                        //   lifetime is nearly as long as `input_device_registry_server`'s.
                        //
                        // Nonetheless, InputDeviceRegistry isn't critical to production use.
                        // So we just log the error and move on.
                        warn!(
                            "failed to forward InputDeviceRegistryRequestStream: {:?}; \
                                must restart to enable input injection",
                            e
                        )
                    }
                }
            }
            ExposedServices::LightSensor(request_stream) => {
                if let Some(light_sensor_server) = light_sensor_server.as_ref() {
                    match light_sensor_server.handle_request(request_stream).await {
                        Ok(()) => (),
                        Err(e) => {
                            warn!(
                                "failed to forward light sensor request via LightSensorRequestStream: {e:?}"
                            );
                        }
                    }
                }
            }
            ExposedServices::MediaButtonsListenerRegistry(request_stream) => {
                match &media_buttons_listener_registry_server.handle_request(request_stream).await {
                    Ok(()) => (),
                    Err(e) => {
                        warn!(
                            "failed to forward media buttons listener request via DeviceListenerRegistryRequestStream: {:?}",
                            e
                        )
                    }
                }
            }
            ExposedServices::FactoryResetCountdown(request_stream) => {
                match &factory_reset_countdown_server.handle_request(request_stream).await {
                    Ok(()) => (),
                    Err(e) => {
                        warn!("failed to forward FactoryResetCountdown: {:?}", e)
                    }
                }
            }
            ExposedServices::FactoryReset(request_stream) => {
                match &factory_reset_device_server.handle_request(request_stream).await {
                    Ok(()) => (),
                    Err(e) => {
                        warn!("failed to forward fuchsia.recovery.policy.Device: {:?}", e)
                    }
                }
            }
            ExposedServices::UserInteractionObservation(stream) => {
                #[cfg(fuchsia_api_level_at_least = "HEAD")]
                {
                    let interaction_state_handler = interaction_state_handler.clone();
                    fasync::Task::local(async move {
                    match interaction_state_handler
                        .handle_interaction_aggregator_request_stream(stream)
                        .await
                    {
                        Ok(()) => (),
                        Err(e) => {
                            warn!(
                        "failure while serving fuchsia.input.interaction.observation.Aggregator: {:?}",
                        e
                    );
                        }}
                    }).detach();
                }
                #[cfg(fuchsia_api_level_less_than = "HEAD")]
                {
                    let _ = stream;
                    error!("scene_manager built without InteractionStateHandler due to stable API level.")
                }
            }
            ExposedServices::UserInteraction(stream) => {
                #[cfg(fuchsia_api_level_at_least = "HEAD")]
                {
                    let interaction_state_handler = interaction_state_handler.clone();
                    fasync::Task::local(async move {
                        match interaction_state_handler.handle_interaction_notifier_request_stream(stream).await
                        {
                            Ok(()) => (),
                            Err(e) => {
                                warn!(
                                    "failure while serving fuchsia.input.interaction.Notifier: {:?}",
                                    e
                                );
                            }
                        }
                    })
                    .detach();
                }
                #[cfg(fuchsia_api_level_less_than = "HEAD")]
                {
                    let _ = stream;
                    error!("scene_manager built without InteractionStateHandler due to stable API level.")
                }
            }
            ExposedServices::GraphicalPresenter(stream) => {
                fasync::Task::local(handle_graphical_presenter_request_stream(
                    stream,
                    Arc::clone(&scene_manager),
                ))
                .detach();
            }
        }
    }

    info!("Finished service handler loop; exiting main.");
    Ok(())
}

pub async fn handle_scene_manager_request_stream(
    mut request_stream: SceneManagerRequestStream,
    scene_manager: Arc<Mutex<dyn SceneManagerTrait>>,
) {
    while let Ok(Some(request)) = request_stream.try_next().await {
        match request {
            SceneManagerRequest::SetRootView { view_provider, responder } => {
                info!("Processing SceneManagerRequest::SetRootView().");
                let proxy = view_provider.into_proxy();
                let mut scene_manager = scene_manager.lock().await;
                let set_root_view_result =
                    scene_manager.set_root_view_deprecated(proxy).await.map_err(|e| {
                        error!("Failed to obtain ViewRef from SetRootView(): {}", e);
                        PresentRootViewError::InternalError
                    });
                if let Err(e) = responder.send(set_root_view_result) {
                    error!("Error responding to SetRootView(): {}", e);
                }
            }
            SceneManagerRequest::PresentRootViewLegacy {
                view_holder_token: _,
                view_ref: _,
                responder,
            } => {
                error!("Unsupported call to fuchsia.session.scene.Manager/PresentRootViewLegacy() (GFX only).");
                if let Err(e) = responder.send(Err(PresentRootViewError::InternalError)) {
                    error!("Error responding to PresentRootViewLegacy(): {}", e);
                }
            }
            SceneManagerRequest::PresentRootView { viewport_creation_token, responder } => {
                info!("Processing SceneManagerRequest::PresentRootView().");
                let mut scene_manager = scene_manager.lock().await;
                let set_root_view_result =
                    scene_manager.set_root_view(viewport_creation_token, None).await.map_err(|e| {
                        error!("Failed to obtain ViewRef from PresentRootView(): {}", e);
                        PresentRootViewError::InternalError
                    });
                if let Err(e) = responder.send(set_root_view_result) {
                    error!("Error responding to PresentRootView(): {}", e);
                }
            }
        };
    }
}

pub async fn handle_graphical_presenter_request_stream(
    mut request_stream: GraphicalPresenterRequestStream,
    scene_manager: Arc<Mutex<dyn SceneManagerTrait>>,
) {
    while let Ok(Some(request)) = request_stream.try_next().await {
        match request {
            GraphicalPresenterRequest::PresentView { view_spec, responder, .. } => {
                match view_spec {
                    ViewSpec {
                        view_holder_token: Some(_),
                        view_ref: _,
                        viewport_creation_token: None,
                        ..
                    } => {
                        error!("Processing fuchsia.element.GraphicalPresenter/PresentView() with GFX view tokens.");
                        if let Err(e) = responder.send(Err(PresentViewError::InvalidArgs)) {
                            error!("Error responding to PresentView(): {}", e);
                        }
                    }
                    ViewSpec {
                        viewport_creation_token: Some(viewport_creation_token),
                        view_holder_token: None,
                        view_ref: None,
                        ..
                    } => {
                        info!("Processing fuchsia.element.GraphicalPresenter/PresentView() with Flatland view tokens.");
                        let mut scene_manager = scene_manager.lock().await;
                        let set_root_view_result = scene_manager
                            .set_root_view(viewport_creation_token, None)
                            .await
                            .map_err(|e| {
                                error!("Failed to PresentView() - Flatland: {}", e);
                                PresentViewError::InvalidArgs
                            });
                        if let Err(e) = responder.send(set_root_view_result) {
                            error!("Error responding to PresentView(): {}", e);
                        }
                    }
                    _ => {
                        error!("Failed to retrieve valid tokens from ViewSpec");
                        if let Err(e) = responder.send(Err(PresentViewError::InvalidArgs)) {
                            error!("Error responding to PresentView(): {}", e);
                        }
                    }
                };
            }
        };
        info!("No longer processing fuchsia.element.GraphicalPresenter request stream.");
    }
}

#[cfg(test)]

mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl::AsHandleRef;
    use fidl_fuchsia_element::GraphicalPresenterMarker;
    use fuchsia_scenic as scenic;
    use scene_management_mocks::MockSceneManager;

    /// Tests that handle_graphical_presenter_request_stream, when receiving a GFX present_view request, errors.
    #[fasync::run_singlethreaded(test)]
    async fn handle_graphical_presenter_request_stream_present_view_gfx_errors() -> Result<(), Error>
    {
        let (proxy, stream) = create_proxy_and_stream::<GraphicalPresenterMarker>();
        let scene_manager = Arc::new(Mutex::new(MockSceneManager::new()));
        let mock_scene_manager = Arc::clone(&scene_manager);
        fasync::Task::local(handle_graphical_presenter_request_stream(stream, mock_scene_manager))
            .detach();

        let view_token_pair = scenic::ViewTokenPair::new()?;
        let view_ref_pair = scenic::ViewRefPair::new()?;
        let view_spec = ViewSpec {
            view_holder_token: Some(view_token_pair.view_holder_token),
            view_ref: Some(view_ref_pair.view_ref),
            ..Default::default()
        };
        if let Err(present_view_result) = proxy
            .present_view(
                view_spec, /* annotation controller */ None, /* view controller */ None,
            )
            .await
            .unwrap()
        {
            assert_eq!(present_view_result, PresentViewError::InvalidArgs);
        } else {
            panic!("Expected an error from present_view().");
        }

        Ok(())
    }

    /// Tests that handle_graphical_presenter_request_stream, when receiving a Flatland present_view request, passes the viewport_creation_token and None to set_root_view().
    #[fasync::run_singlethreaded(test)]
    async fn handle_graphical_presenter_request_stream_presents_view_flatland() -> Result<(), Error>
    {
        let (proxy, stream) = create_proxy_and_stream::<GraphicalPresenterMarker>();
        let scene_manager = Arc::new(Mutex::new(MockSceneManager::new()));
        let mock_scene_manager = Arc::clone(&scene_manager);
        fasync::Task::local(handle_graphical_presenter_request_stream(stream, mock_scene_manager))
            .detach();

        let view_creation_token_pair = scenic::flatland::ViewCreationTokenPair::new()?;
        let expected_viewport_creation_token_koid =
            view_creation_token_pair.viewport_creation_token.value.get_koid();
        let view_spec = ViewSpec {
            viewport_creation_token: Some(view_creation_token_pair.viewport_creation_token),
            ..Default::default()
        };

        let _ = proxy
            .present_view(
                view_spec, /* annotation controller */ None, /* view controller */ None,
            )
            .await;

        let (recorded_viewport_creation_token, recorded_view_ref) =
            scene_manager.lock().await.get_set_root_view_called_args();
        assert_eq!(
            recorded_viewport_creation_token.value.get_koid(),
            expected_viewport_creation_token_koid
        );

        assert_eq!(recorded_view_ref, None);

        Ok(())
    }
}
