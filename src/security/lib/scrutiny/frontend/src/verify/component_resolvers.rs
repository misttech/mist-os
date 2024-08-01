// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use cm_fidl_analyzer::component_instance::ComponentInstanceForAnalyzer;
use cm_fidl_analyzer::component_model::ComponentModelForAnalyzer;
use cm_fidl_analyzer::{BreadthFirstModelWalker, ComponentInstanceVisitor, ComponentModelWalker};
use cm_rust::UseDecl;
use futures::FutureExt;
use moniker::ExtendedMoniker;
use routing::component_instance::{ComponentInstanceInterface, ExtendedInstanceInterface};
use scrutiny_collection::model::DataModel;
use scrutiny_collection::v2_component_model::V2ComponentModel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// ComponentResolversController
///
/// A DataController which returns a list of absolute monikers of all
/// components that, in their environment, contain a resolver with the
///  given moniker for a scheme with access to a protocol.
#[derive(Default)]
pub struct ComponentResolversController {}

/// The expected query format.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct ComponentResolverRequest {
    /// `resolver` URI scheme of interest
    pub scheme: String,
    /// Absolute moniker of the `resolver`
    pub moniker: String,
    /// Filter the results to components resolved with a `resolver` with access to a protocol
    pub protocol: String,
}

/// The response schema.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct ComponentResolverResponse {
    /// Files accessed to perform this query, for depfile generation.
    pub deps: HashSet<PathBuf>,
    /// Component monikers that matched the query.
    pub monikers: Vec<String>,
}

/// Walks the tree for the absolute monikers of all components that,
/// in their environment, contain a resolver with the given moniker
/// for a scheme with access to a protocol.  `monikers` contains the
/// components which match the `request` parameters.
struct ComponentResolversVisitor {
    request: ComponentResolverRequest,
    monikers: Vec<String>,
}

impl ComponentResolversVisitor {
    fn new(request: ComponentResolverRequest) -> Self {
        let monikers = Vec::new();
        Self { request, monikers }
    }

    fn get_monikers(&self) -> Vec<String> {
        self.monikers.clone()
    }

    fn check_instance(&mut self, instance: &Arc<ComponentInstanceForAnalyzer>) -> Result<()> {
        if let Ok(Some((
            ExtendedInstanceInterface::Component(resolver_register_instance),
            resolver,
        ))) = instance.environment().get_registered_resolver(&self.request.scheme)
        {
            // The resolver is a capability and we need to get the component that provides it.
            let resolver_source = match ComponentModelForAnalyzer::route_capability_sync(
                routing::RouteRequest::Resolver(resolver),
                &resolver_register_instance,
            ) {
                (Ok(s), _route) => match s.source.source_moniker() {
                    ExtendedMoniker::ComponentInstance(moniker) => instance
                        .find_absolute(&moniker)
                        .now_or_never()
                        .expect("now_or_never failed to produce a result")
                        .expect("failed to walk to other component instance"),
                    ExtendedMoniker::ComponentManager => {
                        return Err(anyhow!(
                            "The plugin is unable to verify resolvers declared above the root."
                        ));
                    }
                },
                (Err(err), _route) => {
                    eprintln!(
                        "Ignoring invalid resolver configuration for {}: {:#}",
                        instance.moniker(),
                        anyhow!(err).context("failed to route to a resolver")
                    );
                    return Ok(());
                }
            };

            let moniker = moniker::Moniker::parse_str(&self.request.moniker)?;

            if *resolver_source.moniker() == moniker {
                for use_decl in &resolver_source.decl_for_testing().uses {
                    if let UseDecl::Protocol(name) = use_decl {
                        if name.source_name == self.request.protocol {
                            self.monikers.push(instance.moniker().to_string());
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl ComponentInstanceVisitor for ComponentResolversVisitor {
    fn visit_instance(&mut self, instance: &Arc<ComponentInstanceForAnalyzer>) -> Result<()> {
        self.check_instance(instance)
            .with_context(|| format!("while visiting {}", instance.moniker()))
    }
}

impl ComponentResolversController {
    pub fn get_monikers(
        model: Arc<DataModel>,
        request: ComponentResolverRequest,
    ) -> Result<ComponentResolverResponse> {
        let tree_data = model
            .get::<V2ComponentModel>()
            .context("Failed to get V2ComponentModel from ComponentResolversController model")?;
        let deps = tree_data.deps.clone();

        let model = &tree_data.component_model;

        let mut walker = BreadthFirstModelWalker::new();
        let mut visitor = ComponentResolversVisitor::new(request);

        walker.walk(&model, &mut visitor).context(
            "Failed to walk V2ComponentModel with BreadthFirstWalker and ComponentResolversVisitor",
        )?;

        Ok(ComponentResolverResponse { monikers: visitor.get_monikers(), deps })
    }
}
