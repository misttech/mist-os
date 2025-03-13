// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability_source::{AggregateCapability, CapabilitySource, FilteredProviderSource};
use async_trait::async_trait;
use cm_rust::{NameMapping, OfferDecl, OfferServiceDecl};
use router_error::RouterError;
#[cfg(target_os = "fuchsia")]
use sandbox::RemotableCapability;
use sandbox::{
    Capability, Connector, Data, Dict, DirEntry, Request, Routable, Router, RouterResponse,
};

/// A router that will apply renames/filtering on any dictionaries routed through it.
struct ServiceRenameRouter {
    router: Router<DirEntry>,
    // This field is not read on host
    #[allow(dead_code)]
    renames: Vec<NameMapping>,
    offer_service_decl: OfferServiceDecl,
}

#[async_trait]
impl Routable<DirEntry> for ServiceRenameRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<DirEntry>, RouterError> {
        self.handle_dir_router(request, debug).await
    }
}

impl ServiceRenameRouter {
    async fn handle_dir_router(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<DirEntry>, RouterError> {
        let result = self.router.route(request, debug).await;
        match result {
            Ok(RouterResponse::Capability(_source_services_directory)) => {
                #[cfg(target_os = "fuchsia")]
                {
                    let target_services_dict = Dict::new();
                    let dir_ent_ref = std::sync::Arc::new(_source_services_directory);
                    for rename in self.renames.iter() {
                        let path =
                            vfs::path::Path::validate_and_split(format!("{}", &rename.source_name))
                                .expect("path from component manifest is invalid");
                        let sub_node_dir_entry = DirEntry::new(std::sync::Arc::new(
                            vfs::directory::entry::SubNode::new(
                                dir_ent_ref.clone(),
                                path,
                                fidl_fuchsia_io::DirentType::Directory,
                            ),
                        ));
                        target_services_dict
                            .insert(rename.target_name.clone(), sub_node_dir_entry.into())
                            .expect("failed to insert into target services dict");
                    }
                    let dir_entry = DirEntry::new(
                        target_services_dict
                            .try_into_directory_entry(vfs::execution_scope::ExecutionScope::new())
                            .unwrap(),
                    );
                    return Ok(dir_entry.into());
                }
                #[cfg(not(target_os = "fuchsia"))]
                {
                    return Ok(DirEntry {}.into());
                }
            }
            Ok(RouterResponse::Debug(capability_source_data)) => {
                Ok(RouterResponse::Debug(self.wrap_debug_response(capability_source_data)))
            }
            Ok(RouterResponse::Unavailable) => Ok(RouterResponse::Unavailable),
            Err(e) => Err(e),
        }
    }

    /// Converts a capability source into `CapabilitySource::FilteredProvider`
    fn wrap_debug_response(&self, capability_source_data: Data) -> Data {
        log::warn!("wrapping debug response with a filtered provider");
        let capability_source: CapabilitySource = Capability::Data(capability_source_data)
            .try_into()
            .expect("failed to unpersist capability source");
        let capability_source = match capability_source {
            CapabilitySource::Component(source) => {
                CapabilitySource::FilteredProvider(FilteredProviderSource {
                    capability: AggregateCapability::Service(
                        self.offer_service_decl.source_name.clone(),
                    ),
                    moniker: source.moniker.clone(),
                    service_capability: source.capability,
                    offer_service_decl: self.offer_service_decl.clone(),
                })
            }
            CapabilitySource::FilteredProvider(earlier_filtered_provider) => {
                CapabilitySource::FilteredProvider(FilteredProviderSource {
                    capability: AggregateCapability::Service(
                        self.offer_service_decl.source_name.clone(),
                    ),
                    moniker: earlier_filtered_provider.moniker.clone(),
                    service_capability: earlier_filtered_provider.service_capability,
                    offer_service_decl: self.offer_service_decl.clone(),
                })
            }
            other_source => panic!("unexpected source? {:?}", other_source),
        };
        capability_source.try_into().expect("failed to persist capability source")
    }
}

pub trait WithServiceRenamesAndFilter {
    /// When a dictionary is returned through this router a new dictionary will instead be returned
    /// with the entries from the first dictionary, but renamed and filtered as described in
    /// `offer_service_decl`.
    ///
    /// This will be a no-op if `offer` is not `OfferDecl::Service`.
    fn with_service_renames_and_filter(self, offer: OfferDecl) -> Capability;
}

impl WithServiceRenamesAndFilter for Router<Dict> {
    fn with_service_renames_and_filter(self, _offer: OfferDecl) -> Capability {
        self.into()
    }
}

impl WithServiceRenamesAndFilter for Router<Data> {
    fn with_service_renames_and_filter(self, _offer: OfferDecl) -> Capability {
        self.into()
    }
}

impl WithServiceRenamesAndFilter for Router<Connector> {
    fn with_service_renames_and_filter(self, _offer: OfferDecl) -> Capability {
        self.into()
    }
}

impl WithServiceRenamesAndFilter for Router<DirEntry> {
    fn with_service_renames_and_filter(self, offer: OfferDecl) -> Capability {
        let offer_service_decl = match offer {
            OfferDecl::Service(decl) => decl,
            _ => {
                return self.into();
            }
        };
        let renames = process_offer_renames(&offer_service_decl);
        if renames.is_empty() {
            // There are no renames or filters set, rendering as a no-op. Return the underlying
            // router, because we won't do anything.
            return self.into();
        }
        Router::new(ServiceRenameRouter { router: self, renames, offer_service_decl }).into()
    }
}

pub fn process_offer_renames(service_offer_decl: &OfferServiceDecl) -> Vec<NameMapping> {
    match (&service_offer_decl.renamed_instances, &service_offer_decl.source_instance_filter) {
        (Some(renames), Some(filter)) if !renames.is_empty() && !filter.is_empty() => {
            // If rename mappings and a filter are set, we ignore mappings that aren't included
            // in the filter.
            renames
                .into_iter()
                .filter(|mapping| filter.contains(&mapping.target_name))
                .cloned()
                .collect()
        }
        (Some(renames), _) if !renames.is_empty() => renames.clone(),
        (_, Some(filter)) if !filter.is_empty() => {
            // If a filter is set and no renames, we can implement the filter as a set of
            // renames. This helps reduce code duplication.
            filter
                .into_iter()
                .map(|name| NameMapping { source_name: name.clone(), target_name: name.clone() })
                .collect()
        }
        _ => vec![],
    }
}
