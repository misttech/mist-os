// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Cursor;

use anyhow::Error;
use appkit::{
    load_image_from_bytes, load_png, Event, EventSender, Phase, SystemEvent, Window, WindowEvent,
};
use fidl_fuchsia_element as felement;
use fidl_fuchsia_ui_app as ui_app;
use fidl_fuchsia_ui_composition as ui_comp;
use fidl_fuchsia_ui_views as ui_views;
use fuchsia_async as fasync;
use fuchsia_component::{client::connect_to_protocol, server};
use futures::{channel::mpsc::UnboundedReceiver, StreamExt};
use tracing::*;

const NUM_APP_ENTRIES: u32 = 4;
const APP_ENTRY_COMPONENT_URLS: [&'static str; 4] = [
    "fuchsia-pkg://chromium.org/chrome#meta/chrome.cm",
    "fuchsia-pkg://fuchsia.com/spinning-square-rs#meta/spinning-square-rs.cm",
    "fuchsia-pkg://fuchsia.com/terminal#meta/terminal.cm",
    "fuchsia-pkg://fuchsia.com/shell_settings#meta/shell_settings.cm",
];
const LAUNCHER_IMG_WIDTH: u32 = 180;

// LINT.IfChange
// Note: This value should match the APP_LAUNCHER_HEIGHT value in wm package.
const LAUNCHER_IMG_HEIGHT: u32 = 32;
// LINT.ThenChange(../../wm/src/wm.rs)

struct AppLauncher {
    _window: Window,
    element_manager: felement::ManagerProxy,
    _event_sender: EventSender,
}

impl AppLauncher {
    fn new(
        window: Window,
        element_manager: felement::ManagerProxy,
        event_sender: EventSender,
    ) -> Self {
        AppLauncher { _window: window, element_manager, _event_sender: event_sender }
    }

    async fn handle_events(&mut self, mut receiver: UnboundedReceiver<Event>) -> Result<(), Error> {
        while let Some(event) = receiver.next().await {
            debug!("{:?}", event);
            match event {
                Event::WindowEvent { event: WindowEvent::Pointer { event }, .. } => {
                    if event.phase == Phase::Down {
                        if let Some(component_url) =
                            component_url_from_pos(event.logical_x, event.logical_y)
                        {
                            launch_element(self.element_manager.clone(), component_url);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn run(
        event_sender: EventSender,
        mut receiver: UnboundedReceiver<Event>,
    ) -> Result<(), Error> {
        let element_manager = connect_to_protocol::<felement::ManagerMarker>()?;

        let view_creation_token = AppLauncher::get_view_creation_token(&mut receiver).await;
        let mut window =
            Window::new(event_sender.clone()).with_view_creation_token(view_creation_token);
        window.create_view()?;

        static IMAGE_DATA: &'static [u8] = include_bytes!("../app_launcher.png");
        let (bytes, width, height) = load_png(Cursor::new(IMAGE_DATA)).expect("Failed to load png");
        let image_data = load_image_from_bytes(&bytes, width, height).await?;

        let image = window.create_image(image_data)?;
        image.set_size(LAUNCHER_IMG_WIDTH, LAUNCHER_IMG_HEIGHT)?;
        image.set_blend_mode(ui_comp::BlendMode::SrcOver)?;
        window.set_content(window.get_root_transform_id(), image.get_content_id())?;
        window.redraw();

        let mut app = AppLauncher::new(window, element_manager, event_sender);
        app.handle_events(receiver).await
    }

    // Returns the first ViewCreationToken received by ViewProvider service while consuming all
    // other events.
    async fn get_view_creation_token(
        receiver: &mut UnboundedReceiver<Event>,
    ) -> ui_views::ViewCreationToken {
        while let Some(event) = receiver.next().await {
            if let Event::SystemEvent { event: SystemEvent::ViewCreationToken { token } } = event {
                return token;
            }
        }
        unreachable!()
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    info!("Started...");

    let (sender, receiver) = EventSender::new();

    let app_fut = AppLauncher::run(sender.clone(), receiver);

    let services_fut = run_services(sender.clone());

    let _ = futures::join!(app_fut, services_fut);

    info!("Stopped");
    Ok(())
}

enum IncomingService {
    ViewProvider(ui_app::ViewProviderRequestStream),
}

async fn run_services(sender: EventSender) {
    let mut fs = server::ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::ViewProvider);
    fs.take_and_serve_directory_handle().expect("failed to serve directory handle");

    fs.for_each_concurrent(1, |incoming_service| async {
        match incoming_service {
            IncomingService::ViewProvider(mut stream) => {
                while let Some(request) = stream.next().await {
                    match request {
                        Ok(ui_app::ViewProviderRequest::CreateView2 { args, .. }) => {
                            if let Some(view_creation_token) = args.view_creation_token {
                                sender.send(Event::SystemEvent {
                                    event: SystemEvent::ViewCreationToken {
                                        token: view_creation_token,
                                    },
                                });
                            } else {
                                error!("CreateView2() missing view_creation_token field");
                            }
                        }
                        Ok(_) => error!("ViewProvider impl only handles CreateView2"),
                        Err(e) => error!("Failed to read from ViewProviderRequestStream: {:?}", e),
                    }
                }
            }
        }
    })
    .await;
}

fn component_url_from_pos(x: f32, _y: f32) -> Option<String> {
    if x <= LAUNCHER_IMG_WIDTH as f32 {
        let entry_width = (LAUNCHER_IMG_WIDTH / NUM_APP_ENTRIES) as usize;
        let component_url = APP_ENTRY_COMPONENT_URLS.get(x as usize / entry_width)?;
        Some(component_url.to_string())
    } else {
        None
    }
}

fn launch_element(element_manager: felement::ManagerProxy, component_url: String) {
    fasync::Task::local(async move {
        let component_url_clone = component_url.clone();
        let component_url = Some(component_url);
        let spec = felement::Spec { component_url, ..Default::default() };
        match element_manager.propose_element(spec, None).await {
            Ok(_) => info!("Launching {:?}", component_url_clone),
            Err(e) => error!("Failed to launch {:?} error {:?}", component_url_clone, e),
        }
    })
    .detach();
}
