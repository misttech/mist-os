// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::agent::earcons::bluetooth_handler::BluetoothHandler;
use crate::agent::earcons::volume_change_handler::VolumeChangeHandler;
use crate::agent::{
    AgentError, Context as AgentContext, Invocation, InvocationResult, Lifespan, Payload,
};
use crate::event::Publisher;
use crate::service;
use crate::service_context::{ExternalServiceProxy, ServiceContext};
use fidl_fuchsia_media_sounds::PlayerProxy;
use fuchsia_async as fasync;
use futures::lock::Mutex;
use std::collections::HashSet;
use std::fmt::Debug;
use std::rc::Rc;

/// The Earcons Agent is responsible for watching updates to relevant sources that need to play
/// sounds.
pub(crate) struct Agent {
    publisher: Publisher,
    sound_player_connection: Rc<Mutex<Option<ExternalServiceProxy<PlayerProxy>>>>,
    messenger: service::message::Messenger,
}

/// Params that are common to handlers of the earcons agent.
#[derive(Clone)]
pub(super) struct CommonEarconsParams {
    pub(super) service_context: Rc<ServiceContext>,
    pub(super) sound_player_added_files: Rc<Mutex<HashSet<&'static str>>>,
    pub(super) sound_player_connection: Rc<Mutex<Option<ExternalServiceProxy<PlayerProxy>>>>,
}

impl Debug for CommonEarconsParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonEarconsParams")
            .field("sound_player_added_files", &self.sound_player_added_files)
            .field("sound_player_connection", &self.sound_player_connection)
            .finish_non_exhaustive()
    }
}

impl Agent {
    pub(crate) async fn create(mut context: AgentContext) {
        let mut agent = Agent {
            publisher: context.get_publisher(),
            sound_player_connection: Rc::new(Mutex::new(None)),
            messenger: context.create_messenger().await.expect("messenger should be created"),
        };

        fasync::Task::local(async move {
            let _ = &context;
            while let Ok((Payload::Invocation(invocation), client)) =
                context.receptor.next_of::<Payload>().await
            {
                let _ = client.reply(Payload::Complete(agent.handle(invocation).await).into());
            }

            log::info!("Earcons agent done processing requests");
        })
        .detach();
    }

    async fn handle(&mut self, invocation: Invocation) -> InvocationResult {
        // Only process service lifespans.
        if Lifespan::Initialization != invocation.lifespan {
            return Err(AgentError::UnhandledLifespan);
        }

        let common_earcons_params = CommonEarconsParams {
            service_context: invocation.service_context,
            sound_player_added_files: Rc::new(Mutex::new(HashSet::new())),
            sound_player_connection: self.sound_player_connection.clone(),
        };

        if let Err(e) = VolumeChangeHandler::create(
            self.publisher.clone(),
            common_earcons_params.clone(),
            self.messenger.clone(),
        )
        .await
        {
            // For now, report back as an error to prevent issues on
            // platforms that don't support the handler's dependencies.
            // TODO(https://fxbug.dev/42139617): Handle with config
            log::error!("Could not set up VolumeChangeHandler: {:?}", e);
        }

        if BluetoothHandler::create(
            self.publisher.clone(),
            common_earcons_params.clone(),
            self.messenger.clone(),
        )
        .await
        .is_err()
        {
            // For now, report back as an error to prevent issues on
            // platforms that don't support the handler's dependencies.
            // TODO(https://fxbug.dev/42139617): Handle with config
            log::error!("Could not set up BluetoothHandler");
        }

        Ok(())
    }
}
