// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::agent::{AgentError, Context, Invocation, InvocationResult, Lifespan, Payload};
use crate::base::SettingType;
use crate::event::{restore, Event, Publisher};
use crate::handler::base::{Error, Payload as HandlerPayload, Request};
use crate::message::base::Audience;
use crate::service;
use fuchsia_async as fasync;
use std::collections::HashSet;

/// The Restore Agent is responsible for signaling to all components to restore
/// external sources to the last known value. It is invoked during startup.
#[derive(Debug)]
pub(crate) struct RestoreAgent {
    messenger: service::message::Messenger,
    event_publisher: Publisher,
    available_components: HashSet<SettingType>,
}

impl RestoreAgent {
    pub(crate) async fn create(context: Context) {
        let mut agent = RestoreAgent {
            messenger: context.create_messenger().await.expect("should acquire messenger"),
            event_publisher: context.get_publisher(),
            available_components: context.available_components,
        };

        let mut receptor = context.receptor;
        fasync::Task::local(async move {
            while let Ok((Payload::Invocation(invocation), client)) =
                receptor.next_of::<Payload>().await
            {
                let _ = client.reply(Payload::Complete(agent.handle(invocation).await).into());
            }
        })
        .detach();
    }

    async fn handle(&mut self, invocation: Invocation) -> InvocationResult {
        match invocation.lifespan {
            Lifespan::Initialization => {
                for component in &self.available_components {
                    let mut receptor = self.messenger.message(
                        HandlerPayload::Request(Request::Restore).into(),
                        Audience::Address(service::Address::Handler(*component)),
                    );

                    let response = receptor
                        .next_of::<HandlerPayload>()
                        .await
                        .map_err(|e| {
                            log::error!("Received error when getting payload: {:?}", e);
                            AgentError::UnexpectedError
                        })?
                        .0;
                    if let HandlerPayload::Response(response) = response {
                        match response {
                            Ok(_) => {
                                continue;
                            }
                            Err(Error::UnimplementedRequest(setting_type, _)) => {
                                self.event_publisher
                                    .send_event(Event::Restore(restore::Event::NoOp(setting_type)));
                                continue;
                            }
                            Err(Error::UnhandledType(setting_type)) => {
                                log::info!("setting not available for restore: {:?}", setting_type);
                                continue;
                            }
                            e => {
                                log::error!("error during restore for {component:?}: {e:?}");
                                return Err(AgentError::UnexpectedError);
                            }
                        }
                    } else {
                        log::error!("Error because of response: {:?}", response);
                        return Err(AgentError::UnexpectedError);
                    }
                }
            }
            _ => {
                return Err(AgentError::UnhandledLifespan);
            }
        }

        Ok(())
    }
}
