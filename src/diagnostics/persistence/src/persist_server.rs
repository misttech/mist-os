// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Scheduler;
use anyhow::{format_err, Error};
use fidl_fuchsia_diagnostics_persist::{
    DataPersistenceRequest, DataPersistenceRequestStream, PersistResult,
};
use fuchsia_async as fasync;
use futures::StreamExt;
use log::*;
use persistence_config::{ServiceName, Tag};
use std::collections::HashSet;
use std::sync::Arc;

pub struct PersistServerData {
    // Service name that this persist server is hosting.
    service_name: ServiceName,
    // Mapping from a string tag to an archive reader
    // configured to fetch a specific set of selectors.
    tags: HashSet<Tag>,
    // Scheduler that will handle the persist requests
    scheduler: Scheduler,
}

pub(crate) struct PersistServer {
    /// Persist server data.
    data: Arc<PersistServerData>,

    /// Scope in which we spawn the server task.
    scope: fasync::Scope,
}

impl PersistServer {
    pub fn create(
        service_name: ServiceName,
        tags: Vec<Tag>,
        scheduler: Scheduler,
        scope: fasync::Scope,
    ) -> PersistServer {
        let tags = HashSet::from_iter(tags);
        Self { data: Arc::new(PersistServerData { service_name, tags, scheduler }), scope }
    }

    /// Spawn a task to handle requests from components.
    pub fn spawn(&self, stream: DataPersistenceRequestStream) {
        let data = self.data.clone();
        self.scope.spawn(async move {
            if let Err(e) = Self::handle_requests(data, stream).await {
                warn!("error handling persistence request: {e}");
            }
        });
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
