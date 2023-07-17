// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod base;
mod flatland;

use {
    crate::{
        base::{MessageInternal, TilesSession},
        flatland::FlatlandTilesSession,
    },
    anyhow::Error,
    fidl_fuchsia_element as element, fuchsia_async as fasync,
    fuchsia_component::{client::connect_to_protocol, server::ServiceFs, server::ServiceObj},
    futures::{channel::mpsc::UnboundedSender, StreamExt, TryStreamExt},
    tiles_config,
    tracing::{error, warn},
};

// The maximum number of concurrent services to serve.
const NUM_CONCURRENT_REQUESTS: usize = 5;

enum ExposedServices {
    GraphicalPresenter(element::GraphicalPresenterRequestStream),
}

fn expose_services() -> Result<ServiceFs<ServiceObj<'static, ExposedServices>>, Error> {
    let mut fs = ServiceFs::new();

    // Add services for component outgoing directory.
    fs.dir("svc").add_fidl_service(ExposedServices::GraphicalPresenter);
    fs.take_and_serve_directory_handle()?;

    Ok(fs)
}

fn run_services(
    fs: ServiceFs<ServiceObj<'static, ExposedServices>>,
    internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        fs.for_each_concurrent(NUM_CONCURRENT_REQUESTS, |service_request: ExposedServices| async {
            match service_request {
                ExposedServices::GraphicalPresenter(request_stream) => {
                    run_graphical_presenter_service(request_stream, internal_sender.clone());
                }
            }
        })
        .await;
    })
    .detach();
}

fn run_graphical_presenter_service(
    mut request_stream: element::GraphicalPresenterRequestStream,
    internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        while let Ok(Some(request)) = request_stream.try_next().await {
            match request {
                element::GraphicalPresenterRequest::PresentView {
                    view_spec,
                    annotation_controller,
                    view_controller_request,
                    responder,
                } => {
                    // "Unwrap" the optional element::AnnotationControllerProxy.
                    let annotation_controller = match annotation_controller {
                        Some(proxy) => match proxy.into_proxy() {
                            Ok(proxy) => Some(proxy),
                            Err(e) => {
                                warn!("Failed to obtain AnnotationControllerProxy: {}", e);
                                None
                            }
                        },
                        None => None,
                    };
                    // "Unwrap" the optional element::ViewControllerRequestStream.
                    let view_controller_request_stream = match view_controller_request {
                        Some(request_stream) => match request_stream.into_stream() {
                            Ok(request_stream) => Some(request_stream),
                            Err(e) => {
                                warn!("Failed to obtain ViewControllerRequestStream: {}", e);
                                None
                            }
                        },
                        None => None,
                    };
                    internal_sender
                        .unbounded_send(
                            MessageInternal::GraphicalPresenterPresentView {
                                view_spec,
                                annotation_controller,
                                view_controller_request_stream,
                                responder,
                            },
                            // TODO(fxbug.dev/88656): is this a safe expect()?  I think so, since
                            // we're using Task::local() instead of Task::spawn(), so we're on the
                            // same thread as main(), which will keep the receiver end alive until
                            // it exits, at which time the executor will not tick this task again.
                            // Assuming that we verify this understanding, what is the appropriate
                            // way to document this understanding?  Is it so idiomatic it needs no
                            // comment?  We're all Rust n00bs here, so maybe not?
                        )
                        .expect("Failed to send MessageInternal.");
                }
            }
        }
        // TODO(fxbug.dev/88656): if the result of try_next() is Err, we should probably log that instead of
        // silently swallowing it.
    })
    .detach();
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    let (internal_sender, mut internal_receiver) =
        futures::channel::mpsc::unbounded::<MessageInternal>();

    // We start listening for service requests, but don't yet start serving those requests until we
    // we receive confirmation that we are hooked up to the Scene Manager.
    let fs = expose_services()?;

    // Connect to the scene owner and attach our tiles view to it.
    let mut tiles_session: Box<dyn TilesSession> =
        Box::new(FlatlandTilesSession::new(internal_sender.clone()).await?);

    // Serve the FIDL services on the message loop, proxying them into internal messages.
    run_services(fs, internal_sender.clone());

    let config = tiles_config::Config::take_from_startup_handle();
    if !config.main_element_url.is_empty() {
        fasync::Task::local(async move {
            let element_manager = connect_to_protocol::<element::ManagerMarker>()
                .expect("failed to connect to fuchsia.element.Manager");
            element_manager
                .propose_element(
                    element::Spec {
                        component_url: Some(config.main_element_url),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("Failed to propose element.")
                .expect("Failed to propose element.");
        })
        .detach();
    }

    // Process internal messages using the tiles session, then cleanup when done.
    while let Some(message) = internal_receiver.next().await {
        if let Err(e) = tiles_session.handle_message(message).await {
            error!("Error handling message: {e}");
            break;
        }
    }

    Ok(())
}
