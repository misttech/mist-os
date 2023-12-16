// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use fidl::endpoints::ControlHandle;
use fidl_fuchsia_mem as fmem;
use fidl_fuchsia_memory_report as freport;
use fuchsia_zircon as zx;
use futures::TryStreamExt;
use std::sync::Arc;
use zx::AsHandleRef;

use crate::{component::ElfComponentInfo, component_set::id::Id, ComponentSet, Job};

pub struct MemoryReporter {
    components: Arc<ComponentSet>,
}

struct ComponentInfo {
    id: Id,
    job: Arc<Job>,
}

impl MemoryReporter {
    pub(crate) fn new(components: Arc<ComponentSet>) -> MemoryReporter {
        MemoryReporter { components }
    }

    async fn get_attribution(&self) -> freport::Attribution {
        let mut components: Vec<ComponentInfo> = vec![];
        self.components
            .clone()
            .visit(|component: &ElfComponentInfo, id: Id| {
                components.push(ComponentInfo { id, job: component.copy_job() });
            })
            .await;
        freport::Attribution {
            children: Some(
                components
                    .into_iter()
                    .map(|info| freport::Principal {
                        identifier: Some(freport::Identifier::Component(info.id.into())),
                        resources: Some(vec![freport::Resources::Job(
                            info.job.proc().get_koid().unwrap().raw_koid(),
                        )]),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    async fn get_attribution_buffer(&self) -> Result<fmem::Buffer, anyhow::Error> {
        let attribution = self.get_attribution().await;
        let bytes = fidl::persist(&attribution).context("serializing memory attribution")?;
        let vmo = zx::Vmo::create(bytes.len() as u64)?;
        vmo.write(&bytes, 0)?;
        Ok(fmem::Buffer { vmo, size: bytes.len() as u64 })
    }

    pub async fn serve(
        &self,
        mut stream: freport::SnapshotProviderRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                freport::SnapshotProviderRequest::GetAttribution { responder } => {
                    responder.send(self.get_attribution_buffer().await.map_err(|e| {
                        tracing::error!("Error getting attribution buffer: {e}");
                        freport::Error::Internal
                    }))?;
                }
                freport::SnapshotProviderRequest::_UnknownMethod {
                    ordinal,
                    control_handle,
                    ..
                } => {
                    tracing::error!("Invalid request to SnapshotProvider: {ordinal}");
                    control_handle.shutdown_with_epitaph(zx::Status::INVALID_ARGS);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{lifecycle_startinfo, new_elf_runner_for_test};
    use assert_matches::assert_matches;
    use cm_config::SecurityPolicy;
    use fidl_fuchsia_component_runner as fcrunner;
    use fidl_fuchsia_io as fio;
    use freport::{Identifier, Resources};
    use moniker::Moniker;
    use routing::policy::ScopedPolicyChecker;
    use std::sync::Arc;

    /// Test that the ELF runner can tell us about the resources used by the component it runs.
    #[fuchsia::test]
    async fn test_attribute_memory() {
        let (_runtime_dir, runtime_dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<freport::SnapshotProviderMarker>().unwrap();
        runner.serve_memory_reporter(snapshot_request_stream);

        // Run a component.
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::try_from("foo/bar").unwrap(),
        ));
        let (_controller, server_controller) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentControllerMarker>()
                .expect("could not create component controller endpoints");
        runner.start(start_info, server_controller).await;

        // Ask about the memory usage of components.
        let attribution_buffer = snapshot_provider.get_attribution().await.unwrap().unwrap();
        let bytes = attribution_buffer.vmo.read_to_vec(0, attribution_buffer.size).unwrap();
        let attribution: freport::Attribution = fidl::unpersist(&bytes).unwrap();

        // It should contain one component, the one we just launched.
        let children = attribution.children.unwrap();
        assert_eq!(children.len(), 1);
        let component = children.first().unwrap();
        assert_matches!(
            &component.identifier,
            Some(Identifier::Component(name))
            if name == "foo/bar, 0"
        );
        // Its resource is a single job.
        assert_matches!(component.resources.as_ref().unwrap().as_slice(), [Resources::Job(_)]);
    }
}
