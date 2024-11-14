// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{constants, Scheduler};
use fidl_fuchsia_diagnostics_persist::{
    DataPersistenceRequest, DataPersistenceRequestStream, PersistResult,
};
use fuchsia_async::TaskGroup;
use fuchsia_component::server::{ServiceFs, ServiceObj};
use fuchsia_sync::Mutex;
use futures::StreamExt;
use persistence_config::{ServiceName, Tag};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::*;

pub struct PersistServerData {
    // Service name that this persist server is hosting.
    service_name: ServiceName,
    // Mapping from a string tag to an archive reader
    // configured to fetch a specific set of selectors.
    tags: HashSet<Tag>,
    // Scheduler that will handle the persist requests
    scheduler: Scheduler,
}

#[derive(Clone)]
pub(crate) struct PersistServer(Arc<Mutex<PersistServerData>>);

impl PersistServer {
    pub fn create(
        service_name: ServiceName,
        tags: Vec<Tag>,
        scheduler: Scheduler,
    ) -> PersistServer {
        let tags = HashSet::from_iter(tags);
        PersistServer(Arc::new(Mutex::new(PersistServerData { service_name, tags, scheduler })))
    }

    // Serve the Persist FIDL protocol.
    pub fn launch_server(
        self,
        task_holder: Arc<Mutex<TaskGroup>>,
        fs: &mut ServiceFs<ServiceObj<'static, ()>>,
    ) {
        let unique_service_name =
            format!("{}-{}", constants::PERSIST_SERVICE_NAME_PREFIX, self.0.lock().service_name);

        let this = self;
        fs.dir("svc").add_fidl_service_at(
            unique_service_name,
            move |mut stream: DataPersistenceRequestStream| {
                let this = this.clone();
                task_holder.lock().spawn(async move {
                    while let Some(Ok(request)) = stream.next().await {
                        let this = this.0.lock();
                        match request {
                            DataPersistenceRequest::Persist { tag, responder, .. } => {
                                let response = if let Ok(tag) = Tag::new(tag) {
                                    if this.tags.contains(&tag) {
                                        this.scheduler.schedule(&this.service_name, vec![tag]);
                                        PersistResult::Queued
                                    } else {
                                        PersistResult::BadName
                                    }
                                } else {
                                    PersistResult::BadName
                                };
                                responder.send(response).unwrap_or_else(|err| {
                                    warn!("Failed to respond {:?} to client: {}", response, err)
                                });
                            }
                            DataPersistenceRequest::PersistTags { tags, responder, .. } => {
                                let (response, tags) = this.validate_tags(&tags);
                                if !tags.is_empty() {
                                    this.scheduler.schedule(&this.service_name, tags);
                                }
                                responder.send(&response).unwrap_or_else(|err| {
                                    warn!("Failed to respond {:?} to client: {}", response, err)
                                });
                            }
                        }
                    }
                });
            },
        );
    }
}

impl PersistServerData {
    fn validate_tags(&self, tags: &[String]) -> (Vec<PersistResult>, Vec<Tag>) {
        let mut response = vec![];
        let mut good_tags = vec![];
        for tag in tags.iter() {
            if let Ok(tag) = Tag::new(tag.to_string()) {
                if self.tags.contains(&tag) {
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
}
