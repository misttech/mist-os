// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Scheduler;
use anyhow::{format_err, Error};
use fidl::endpoints;
use fidl_fuchsia_diagnostics_persist::{
    DataPersistenceMarker, DataPersistenceRequest, DataPersistenceRequestStream, PersistResult,
};
use futures::{StreamExt, TryStreamExt};
use log::*;
use persistence_config::{ServiceName, Tag};
use std::collections::HashSet;
use std::sync::Arc;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

pub struct PersistServerData {
    // Service name that this persist server is hosting.
    service_name: ServiceName,
    // Mapping from a string tag to an archive reader
    // configured to fetch a specific set of selectors.
    tags: HashSet<Tag>,
    // Scheduler that will handle the persist requests
    scheduler: Scheduler,
}

/// PersistServer handles all requests for a single persistence service.
pub(crate) struct PersistServer;

impl PersistServer {
    /// Spawn a task to handle requests from components through a dynamic dictionary.
    pub fn spawn(
        service_name: ServiceName,
        tags: Vec<Tag>,
        scheduler: Scheduler,
        scope: &fasync::Scope,
        requests: fsandbox::ReceiverRequestStream,
    ) {
        let tags = HashSet::from_iter(tags);
        let data = Arc::new(PersistServerData { service_name, tags, scheduler });

        let scope_handle = scope.to_handle();
        scope.spawn(Self::accept_connections(data, requests, scope_handle));
    }

    async fn accept_connections(
        data: Arc<PersistServerData>,
        mut stream: fsandbox::ReceiverRequestStream,
        scope: fasync::ScopeHandle,
    ) {
        while let Some(request) = stream.try_next().await.unwrap() {
            match request {
                fsandbox::ReceiverRequest::Receive { channel, control_handle: _ } => {
                    let data = data.clone();
                    scope.spawn(async move {
                        let server_end =
                            endpoints::ServerEnd::<DataPersistenceMarker>::new(channel);
                        let stream: DataPersistenceRequestStream = server_end.into_stream();
                        if let Err(e) = Self::handle_requests(data, stream).await {
                            warn!("error handling persistence request: {e}");
                        }
                    });
                }
                fsandbox::ReceiverRequest::_UnknownMethod { ordinal, .. } => {
                    warn!(ordinal:%; "Unknown Receiver request");
                }
            }
        }
    }

    async fn handle_requests(
        data: Arc<PersistServerData>,
        mut stream: DataPersistenceRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.next().await {
            let request =
                request.map_err(|e| format_err!("error handling persistence request: {e:?}"))?;

            match request {
                DataPersistenceRequest::Persist { tag, responder, .. } => {
                    let response = if let Ok(tag) = Tag::new(tag) {
                        if data.tags.contains(&tag) {
                            data.scheduler.schedule(&data.service_name, vec![tag]);
                            PersistResult::Queued
                        } else {
                            PersistResult::BadName
                        }
                    } else {
                        PersistResult::BadName
                    };
                    responder.send(response).map_err(|err| {
                        format_err!("Failed to respond {:?} to client: {}", response, err)
                    })?;
                }
                DataPersistenceRequest::PersistTags { tags, responder, .. } => {
                    let (response, tags) = validate_tags(&data.tags, &tags);
                    if !tags.is_empty() {
                        data.scheduler.schedule(&data.service_name, tags);
                    }
                    responder.send(&response).map_err(|err| {
                        format_err!("Failed to respond {:?} to client: {}", response, err)
                    })?;
                }
            }
        }
        Ok(())
    }
}

fn validate_tags(service_tags: &HashSet<Tag>, tags: &[String]) -> (Vec<PersistResult>, Vec<Tag>) {
    let mut response = vec![];
    let mut good_tags = vec![];
    for tag in tags.iter() {
        if let Ok(tag) = Tag::new(tag.to_string()) {
            if service_tags.contains(&tag) {
                response.push(PersistResult::Queued);
                good_tags.push(tag);
            } else {
                response.push(PersistResult::BadName);
                warn!("Tag '{}' was requested but is not configured", tag);
            }
        } else {
            response.push(PersistResult::BadName);
            warn!("Tag '{}' was requested but is not a valid tag string", tag);
        }
    }
    (response, good_tags)
}
