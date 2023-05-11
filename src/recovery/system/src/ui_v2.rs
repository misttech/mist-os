// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use carnelian::{
    app::Config, drawing::DisplayRotation, App, AppAssistant, AppAssistantPtr, AppSender,
    AssistantCreator, AssistantCreatorFunc, ViewAssistantPtr, ViewKey,
};
use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl_fuchsia_recovery_ui as frui;
use fuchsia_async as fasync;
use futures::TryStreamExt;
use ota_lib::OtaComponent;
use recovery_ui::proxy_view_assistant::ProxyViewAssistant;
use recovery_ui::screens::Screens;
use recovery_ui_config::Config as UiConfig;
use recovery_util::crash::{CrashReportBuilder, CrashReporter};
use recovery_util::ota::action::Action;
use recovery_util::ota::controller::{Controller, ControllerImpl, SendEvent};
use recovery_util::ota::state_machine::{Event, OtaStatus, State, StateMachine};
use std::rc::Rc;
use std::sync::Arc;

#[cfg(feature = "debug_console")]
use {
    carnelian::{make_message, MessageTarget},
    futures::channel::mpsc::{self, Sender},
    futures::StreamExt,
    ota_lib::{OtaLogListener, OtaLogListenerImpl},
    recovery_ui::console::{ConsoleMessages, ConsoleViewAssistant},
    recovery_ui::font,
    recovery_util::ota::controller::EventSender,
};

struct RecoveryAppAssistant {
    app_sender: AppSender,
    display_rotation: DisplayRotation,
    controller: Box<dyn Controller>,
    crash_reporter: Option<Rc<CrashReporter>>,
}

impl RecoveryAppAssistant {
    pub fn new(app_sender: AppSender) -> Self {
        let display_rotation = Self::get_display_rotation();
        let crash_reporter = CrashReportBuilder::new().build().unwrap();
        Self::new_with_params(
            app_sender,
            display_rotation,
            Box::new(ControllerImpl::new()),
            Some(crash_reporter),
        )
    }

    pub fn new_with_params(
        app_sender: AppSender,
        display_rotation: DisplayRotation,
        controller: Box<dyn Controller>,
        crash_reporter: Option<Rc<CrashReporter>>,
    ) -> Self {
        Self { app_sender, display_rotation, controller, crash_reporter }
    }

    fn get_display_rotation() -> DisplayRotation {
        let config = UiConfig::take_from_startup_handle();
        match config.display_rotation {
            0 => DisplayRotation::Deg0,
            180 => DisplayRotation::Deg180,
            // Carnelian uses an inverted z-axis for rotation
            90 => DisplayRotation::Deg270,
            270 => DisplayRotation::Deg90,
            val => {
                eprintln!("Invalid display_rotation {}, defaulting to 90 degrees", val);
                DisplayRotation::Deg90
            }
        }
    }

    // TODO(b/253084947) Swap out interface when we switch to the official progress fidl from SWD
    fn send_ota_status(mut event_sender: Box<dyn SendEvent>, percent: f32, status: frui::Status) {
        match status {
            frui::Status::Active => {
                println!("OTA update is now in progress... ({}%)", percent);
                event_sender.send(Event::Progress(percent as u8));
            }
            frui::Status::Complete => {
                event_sender.send(Event::ProgressComplete(OtaStatus::Succeeded))
            }
            frui::Status::Cancelled | frui::Status::Error => {
                // Cancelled and Error are both considered failed, since you can't cancel recovery installation
                event_sender.send(Event::ProgressComplete(OtaStatus::Failed))
            }
            frui::Status::Paused => (), //ignore
        }
    }
}

#[cfg(feature = "debug_console")]
impl RecoveryAppAssistant {
    fn setup_event_logging_observer(&self, view_key: ViewKey) -> Sender<Event> {
        let app_sender = self.app_sender.clone();
        let view_key = view_key.clone();
        let (observer_sender, mut observer_receiver) = mpsc::channel::<Event>(10);

        fasync::Task::local(async move {
            loop {
                match observer_receiver.next().await {
                    Some(Event::DebugLog(msg)) => {
                        app_sender.queue_message(
                            MessageTarget::View(view_key),
                            make_message(ConsoleMessages::AddText(msg)),
                        );
                    }
                    Some(Event::Error(msg)) => {
                        app_sender.queue_message(
                            MessageTarget::View(view_key),
                            make_message(ConsoleMessages::AddText(format!("Error: {}", msg))),
                        );
                    }
                    _ => (), // Ignore other Events
                }
            }
        })
        .detach();
        observer_sender
    }

    fn set_up_ota_logger(&mut self, mut event_sender: EventSender) -> Result<(), Error> {
        let ota_log_listener = OtaLogListenerImpl::new().unwrap();

        fasync::Task::local(async move {
            let mut local_event_sender = event_sender.clone();
            let result = ota_log_listener
                .listen(Box::new(move |line| {
                    local_event_sender.send(Event::DebugLog(format!("OTA: {}", line)))
                }))
                .await;

            if let Err(e) = result {
                let msg = format!("failed to subscribe to OTA syslog: {:?}", e);
                eprintln!("{}", &msg);
                event_sender.send(Event::Error(msg));
            }
        })
        .detach();
        Ok(())
    }
}

impl AppAssistant for RecoveryAppAssistant {
    fn setup(&mut self) -> Result<(), Error> {
        // This line is required for the CQ build and test system, specifically boot_test.go
        println!("recovery: AppAssistant setup");
        Ok(())
    }

    // Create the State Machine
    fn create_view_assistant(&mut self, view_key: ViewKey) -> Result<ViewAssistantPtr, Error> {
        // TODO(b/244744635) Add a structured initialization flow for the recovery component
        let ota_manager =
            Arc::new(OtaComponent::new().expect("failed to create OTA component manager"));
        let action = Action::new(
            self.controller.get_event_sender(),
            ota_manager,
            self.crash_reporter.clone(),
        );
        self.controller.add_state_handler(Box::new(action));

        let screens = Screens::new(self.app_sender.clone(), view_key);
        let first_screen = screens.initial_screen();
        self.controller.add_state_handler(Box::new(screens));

        let state_machine = Box::new(StateMachine::new(State::Home));

        #[cfg(feature = "debug_console")]
        let console_view_assistant_ptr: Option<ViewAssistantPtr> = {
            // Debug Console setup
            self.controller.add_event_observer(self.setup_event_logging_observer(view_key));
            self.set_up_ota_logger(self.controller.get_event_sender().clone())?;

            let font_face = font::get_default_font_face();
            Some(Box::new(ConsoleViewAssistant::new(font_face.clone())?))
        };
        #[cfg(not(feature = "debug_console"))]
        let console_view_assistant_ptr: Option<ViewAssistantPtr> = None;
        let proxy_ptr = Box::new(ProxyViewAssistant::new(
            Some(Box::new(self.controller.get_event_sender())),
            console_view_assistant_ptr,
            first_screen,
        )?);

        self.controller.start(state_machine);
        Ok(proxy_ptr)
    }

    fn filter_config(&mut self, config: &mut Config) {
        config.display_rotation = self.display_rotation;
    }

    fn outgoing_services_names(&self) -> Vec<&'static str> {
        vec![frui::ProgressRendererMarker::PROTOCOL_NAME]
    }

    fn handle_service_connection_request(
        &mut self,
        service_name: &str,
        channel: fasync::Channel,
    ) -> Result<(), Error> {
        match service_name {
            frui::ProgressRendererMarker::PROTOCOL_NAME => {
                let event_sender = self.controller.get_event_sender();

                fasync::Task::local(async move {
                    let mut stream = frui::ProgressRendererRequestStream::from_channel(channel);
                    while let Ok(Some(request)) = stream.try_next().await {
                        match request {
                            frui::ProgressRendererRequest::Render {
                                status,
                                percent_complete,
                                responder,
                            } => {
                                println!("Ota status event {:?}", &status);
                                Self::send_ota_status(
                                    Box::new(event_sender.clone()),
                                    percent_complete,
                                    status,
                                );
                                responder.send().expect("Error replying to progress update");
                            }
                            frui::ProgressRendererRequest::Render2 { payload, responder } => {
                                println!("Ota progress event: {:?}", &payload);
                                if let Some(status) = payload.status {
                                    let progress = payload.percent_complete.unwrap_or(0f32);
                                    Self::send_ota_status(
                                        Box::new(event_sender.clone()),
                                        progress,
                                        status,
                                    );
                                }
                                responder.send().expect("Error replying to progress update");
                            }
                        }
                    }
                })
                .detach();
            }
            _ => panic!("Error: Unexpected service: {}", service_name),
        }
        Ok(())
    }
}

fn make_app_assistant_fut() -> impl FnOnce(&AppSender) -> AssistantCreator<'_> {
    move |app_sender: &AppSender| {
        let f = async move {
            // Route stdout to debuglog, allowing log lines to appear over serial.
            stdout_to_debuglog::init().await.unwrap_or_else(|error| {
                eprintln!("Failed to initialize debuglog: {:?}", error);
            });

            if let Err(e) = recovery_util::regulatory::set_region_code_from_factory().await {
                eprintln!("error setting region code: {:?}", e);
            }
            println!("Starting New recovery UI");

            let assistant = Box::new(RecoveryAppAssistant::new(app_sender.clone()));
            Ok::<AppAssistantPtr, Error>(assistant)
        };
        Box::pin(f)
    }
}

fn make_app_assistant() -> AssistantCreatorFunc {
    Box::new(make_app_assistant_fut())
}

#[allow(unused)]
pub fn main() -> Result<(), Error> {
    App::run(make_app_assistant())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::DiscoverableProtocolMarker;
    use fidl_fuchsia_recovery_ui::{
        ProgressRendererMarker, ProgressRendererRender2Request, Status,
    };
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use ota_lib::OtaStatus;
    use recovery_util::ota::controller::{EventSender, MockController};

    #[fuchsia::test]
    async fn test_create_view_assistant_sets_up_state_handlers_and_starts() {
        let (sender, _) = mpsc::channel::<Event>(10);
        let mut controller = MockController::new();
        controller.expect_get_event_sender().return_const(EventSender::new(sender));
        controller.expect_add_state_handler().times(2).return_const(());
        controller.expect_start().once().return_const(());
        #[cfg(feature = "debug_console")]
        controller.expect_add_event_observer().once().return_const(());

        let mut recovery_app_assistant = RecoveryAppAssistant::new_with_params(
            AppSender::new_for_testing_purposes_only(),
            DisplayRotation::Deg0,
            Box::new(controller),
            None,
        );

        recovery_app_assistant.create_view_assistant(ViewKey::default()).unwrap();
    }

    #[fuchsia::test]
    async fn test_progress_updates_reports_success_to_ota_manager() {
        let (sender, mut on_handle_event) = mpsc::channel::<Event>(10);
        let mut controller = MockController::new();
        controller.expect_get_event_sender().return_const(EventSender::new(sender));
        let mut recovery_app_assistant = RecoveryAppAssistant::new_with_params(
            AppSender::new_for_testing_purposes_only(),
            DisplayRotation::Deg0,
            Box::new(controller),
            None,
        );

        assert_eq!(
            vec![ProgressRendererMarker::PROTOCOL_NAME],
            recovery_app_assistant.outgoing_services_names()
        );

        let (progress_proxy, progress_server) =
            fidl::endpoints::create_proxy::<ProgressRendererMarker>().unwrap();

        recovery_app_assistant
            .handle_service_connection_request(
                ProgressRendererMarker::PROTOCOL_NAME,
                fasync::Channel::from_channel(progress_server.into_channel()).unwrap(),
            )
            .unwrap();

        progress_proxy
            .render2(&ProgressRendererRender2Request {
                status: Some(Status::Active),
                percent_complete: Some(0.0),
                ..Default::default()
            })
            .await
            .unwrap();
        progress_proxy
            .render2(&ProgressRendererRender2Request {
                status: Some(Status::Complete),
                percent_complete: Some(100.0),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(Event::Progress(0), on_handle_event.try_next().unwrap().unwrap());
        assert_eq!(
            Event::ProgressComplete(OtaStatus::Succeeded),
            on_handle_event.try_next().unwrap().unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_progress_updates_reports_error_to_ota_manager() {
        let (sender, mut on_handle_event) = mpsc::channel::<Event>(10);
        let mut controller = MockController::new();
        controller.expect_get_event_sender().return_const(EventSender::new(sender));

        let mut recovery_app_assistant = RecoveryAppAssistant::new_with_params(
            AppSender::new_for_testing_purposes_only(),
            DisplayRotation::Deg0,
            Box::new(controller),
            None,
        );

        assert_eq!(
            vec![ProgressRendererMarker::PROTOCOL_NAME],
            recovery_app_assistant.outgoing_services_names()
        );

        let (progress_proxy, progress_server) =
            fidl::endpoints::create_proxy::<ProgressRendererMarker>().unwrap();

        recovery_app_assistant
            .handle_service_connection_request(
                ProgressRendererMarker::PROTOCOL_NAME,
                fasync::Channel::from_channel(progress_server.into_channel()).unwrap(),
            )
            .unwrap();

        progress_proxy
            .render2(&ProgressRendererRender2Request {
                status: Some(Status::Active),
                percent_complete: Some(0.0),
                ..Default::default()
            })
            .await
            .unwrap();
        progress_proxy
            .render2(&ProgressRendererRender2Request {
                status: Some(Status::Error),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(Event::Progress(0), on_handle_event.try_next().unwrap().unwrap());
        assert_eq!(
            Event::ProgressComplete(OtaStatus::Failed),
            on_handle_event.try_next().unwrap().unwrap()
        );
    }
}
