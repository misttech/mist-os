// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::features::{Feature, FeatureSet};
use crate::{
    offer_to_all_would_duplicate, AnyRef, Availability, Capability, CapabilityClause,
    CapabilityFromRef, CapabilityId, Child, Collection, ConfigKey, ConfigType, ConfigValueType,
    DependencyType, DictionaryRef, Disable, Document, Environment, EnvironmentExtends,
    EnvironmentRef, Error, EventScope, Expose, ExposeFromRef, ExposeToRef, FromClause, Name, Offer,
    OfferFromRef, OfferToRef, OneOrMany, Program, RegistrationRef, Rights, RootDictionaryRef,
    SourceAvailability, Use, UseFromRef,
};
use cm_types::IterablePath;
use directed_graph::DirectedGraph;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::path::Path;
use std::{fmt, iter};

#[derive(Default, Clone)]
pub struct ProtocolRequirements<'a> {
    pub must_offer: &'a [String],
    pub must_use: &'a [String],
}

/// Validates a given cml.
pub(crate) fn validate_cml(
    document: &Document,
    file: Option<&Path>,
    features: &FeatureSet,
    protocol_requirements: &ProtocolRequirements<'_>,
) -> Result<(), Error> {
    let mut ctx = ValidationContext::new(&document, features, protocol_requirements);
    let mut res = ctx.validate();
    if let Err(Error::Validate { filename, .. }) = &mut res {
        if let Some(file) = file {
            *filename = Some(file.to_string_lossy().into_owned());
        }
    }
    res
}

fn offer_can_have_dependency(offer: &Offer) -> bool {
    offer.directory.is_some() || offer.protocol.is_some() || offer.service.is_some()
}

fn offer_dependency(offer: &Offer) -> DependencyType {
    offer.dependency.clone().unwrap_or(DependencyType::Strong)
}
struct ValidationContext<'a> {
    document: &'a Document,
    features: &'a FeatureSet,
    protocol_requirements: &'a ProtocolRequirements<'a>,
    all_children: HashMap<&'a Name, &'a Child>,
    all_collections: HashSet<&'a Name>,
    all_storages: HashMap<&'a Name, &'a CapabilityFromRef>,
    all_services: HashSet<&'a Name>,
    all_protocols: HashSet<&'a Name>,
    all_directories: HashSet<&'a Name>,
    all_runners: HashSet<&'a Name>,
    all_resolvers: HashSet<&'a Name>,
    all_dictionaries: HashMap<&'a Name, &'a Capability>,
    all_configs: HashSet<&'a Name>,
    all_environment_names: HashSet<&'a Name>,
    all_capability_names: HashSet<&'a Name>,
    strong_dependencies: DirectedGraph<DependencyNode<'a>>,
}

// Facet key for fuchsia.test
const TEST_FACET_KEY: &'static str = "fuchsia.test";

// Facet key for deprecated-allowed-packages.
const TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: &'static str = "deprecated-allowed-packages";

// Facet key for type.
const TEST_TYPE_FACET_KEY: &'static str = "type";

impl<'a> ValidationContext<'a> {
    fn new(
        document: &'a Document,
        features: &'a FeatureSet,
        protocol_requirements: &'a ProtocolRequirements<'a>,
    ) -> Self {
        ValidationContext {
            document,
            features,
            protocol_requirements,
            all_children: HashMap::new(),
            all_collections: HashSet::new(),
            all_storages: HashMap::new(),
            all_services: HashSet::new(),
            all_protocols: HashSet::new(),
            all_directories: HashSet::new(),
            all_runners: HashSet::new(),
            all_resolvers: HashSet::new(),
            all_dictionaries: HashMap::new(),
            all_configs: HashSet::new(),
            all_environment_names: HashSet::new(),
            all_capability_names: HashSet::new(),
            strong_dependencies: DirectedGraph::new(),
        }
    }

    fn validate(&mut self) -> Result<(), Error> {
        // Ensure child components, collections, and storage don't use the
        // same name.
        //
        // We currently have the ability to distinguish between storage and
        // children/collections based on context, but still enforce name
        // uniqueness to give us flexibility in future.
        let all_children_names =
            self.document.all_children_names().into_iter().zip(iter::repeat("children"));
        let all_collection_names =
            self.document.all_collection_names().into_iter().zip(iter::repeat("collections"));
        let all_storage_names =
            self.document.all_storage_names().into_iter().zip(iter::repeat("storage"));
        let all_runner_names =
            self.document.all_runner_names().into_iter().zip(iter::repeat("runners"));
        let all_resolver_names =
            self.document.all_resolver_names().into_iter().zip(iter::repeat("resolvers"));
        let all_environment_names =
            self.document.all_environment_names().into_iter().zip(iter::repeat("environments"));
        let all_dictionary_names =
            self.document.all_dictionary_names().into_iter().zip(iter::repeat("dictionaries"));
        ensure_no_duplicate_names(
            all_children_names
                .chain(all_collection_names)
                .chain(all_storage_names)
                .chain(all_runner_names)
                .chain(all_resolver_names)
                .chain(all_environment_names)
                .chain(all_dictionary_names),
        )?;

        // Populate the sets of children and collections.
        if let Some(children) = &self.document.children {
            self.all_children = children.iter().map(|c| (&c.name, c)).collect();
        }
        self.all_collections = self.document.all_collection_names().into_iter().collect();
        self.all_storages = self.document.all_storage_with_sources();
        self.all_services = self.document.all_service_names().into_iter().collect();
        self.all_protocols = self.document.all_protocol_names().into_iter().collect();
        self.all_directories = self.document.all_directory_names().into_iter().collect();
        self.all_runners = self.document.all_runner_names().into_iter().collect();
        self.all_resolvers = self.document.all_resolver_names().into_iter().collect();
        self.all_dictionaries = self.document.all_dictionaries().into_iter().collect();
        self.all_configs = self.document.all_config_names().into_iter().collect();
        self.all_environment_names = self.document.all_environment_names().into_iter().collect();
        self.all_capability_names = self.document.all_capability_names();

        // Validate "children".
        if let Some(children) = &self.document.children {
            for child in children {
                self.validate_child(&child)?;
            }
        }

        // Validate "collections".
        if let Some(collections) = &self.document.collections {
            for collection in collections {
                self.validate_collection(&collection)?;
            }
        }

        // Validate "capabilities".
        if let Some(capabilities) = self.document.capabilities.as_ref() {
            let mut used_ids = HashMap::new();
            for capability in capabilities {
                self.validate_capability(capability, &mut used_ids)?;
            }
        }

        // Validate "use".
        let mut uses_runner = false;
        if let Some(uses) = self.document.r#use.as_ref() {
            let mut used_ids = HashMap::new();
            for use_ in uses.iter() {
                self.validate_use(&use_, &mut used_ids)?;
                if use_.runner.is_some() {
                    uses_runner = true;
                }
            }
        }

        // Validate "expose".
        if let Some(exposes) = self.document.expose.as_ref() {
            let mut used_ids = HashMap::new();
            for expose in exposes.iter() {
                self.validate_expose(&expose, &mut used_ids)?;
            }
        }

        // Validate "offer".
        if let Some(offers) = self.document.offer.as_ref() {
            let mut used_ids = HashMap::new();
            let offered_to_all = offers
                .iter()
                .filter(|o| matches!(o.to, OneOrMany::One(OfferToRef::All)))
                .filter(|o| o.protocol.is_some())
                .collect::<Vec<&Offer>>();

            let mut duplicate_check: HashSet<CapabilityId<'a>> = HashSet::new();
            let problem_protocols = offered_to_all
                .iter()
                .map(|o| CapabilityId::from_offer_expose(*o))
                .collect::<Result<Vec<Vec<CapabilityId<'a>>>, _>>()?
                .into_iter()
                .flatten()
                .filter(|cap_id| !duplicate_check.insert((*cap_id).clone()))
                .collect::<Vec<_>>();
            if !problem_protocols.is_empty() {
                return Err(Error::validate(format!(
                    r#"Protocol(s) {:?} offered to "all" multiple times"#,
                    problem_protocols
                        .iter()
                        .map(|p| match p {
                            CapabilityId::Protocol(name) => name.as_str(),
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>()
                )));
            }

            for offer in offers.iter() {
                self.validate_offer(&offer, &mut used_ids, &offered_to_all)?;
            }
        }

        if uses_runner {
            // Component "use"s a runner. Ensure we don't also have a runner specified in "program",
            // which would necessarily conflict.
            self.validate_runner_not_specified(self.document.program.as_ref())?;
        } else {
            // Component doesn't "use" a runner. Ensure we don't have a component with a "program"
            // block which fails to specify a runner.
            self.validate_runner_specified(self.document.program.as_ref())?;
        }

        // Validate "environments".
        if let Some(environments) = &self.document.environments {
            for env in environments {
                self.validate_environment(&env)?;
            }
        }

        // Validate "config"
        self.validate_config(&self.document.config)?;

        // Check for dependency cycles
        match self.strong_dependencies.topological_sort() {
            Ok(_) => {}
            Err(e) => {
                return Err(Error::validate(format!(
                    "Strong dependency cycles were found. Break the cycle by removing a dependency or marking an offer as weak. Cycles: {}", e.format_cycle())));
            }
        }

        // Check that required offers are present
        self.validate_required_offer_decls()?;

        // Check that required use decls are present
        self.validate_required_use_decls()?;

        self.validate_facets()?;

        Ok(())
    }

    fn get_test_facet(&self) -> Option<&serde_json::Value> {
        match &self.document.facets {
            Some(m) => m.get(TEST_FACET_KEY),
            None => None,
        }
    }

    fn validate_facets(&self) -> Result<(), Error> {
        let test_facet_map = {
            let test_facet = self.get_test_facet();
            match &test_facet {
                None => None,
                Some(serde_json::Value::Object(m)) => Some(m),
                Some(facet) => {
                    return Err(Error::validate(format!(
                        "'{TEST_FACET_KEY}' is not an object: {facet:?}"
                    )))
                }
            }
        };

        let restrict_test_type = self.features.has(&Feature::RestrictTestTypeInFacet);
        let enable_allow_non_hermetic_packages =
            self.features.has(&Feature::EnableAllowNonHermeticPackagesFeature);

        if restrict_test_type {
            let test_type = test_facet_map.map(|m| m.get(TEST_TYPE_FACET_KEY)).flatten();
            if test_type.is_some() {
                return Err(Error::validate(format!(
                    "'{}' is not a allowed in facets. Refer \
https://fuchsia.dev/fuchsia-src/development/testing/components/test_runner_framework?hl=en#non-hermetic_tests \
to run your test in the correct test realm.", TEST_TYPE_FACET_KEY)));
            }
        }

        if enable_allow_non_hermetic_packages {
            let allow_non_hermetic_packages = self.features.has(&Feature::AllowNonHermeticPackages);
            let deprecated_allowed_packages = test_facet_map
                .map_or(false, |m| m.contains_key(TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY));
            if deprecated_allowed_packages && !allow_non_hermetic_packages {
                return Err(Error::validate(format!(
                    "restricted_feature '{}' should be present with facet '{}'",
                    Feature::AllowNonHermeticPackages,
                    TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY
                )));
            }
            if allow_non_hermetic_packages && !deprecated_allowed_packages {
                return Err(Error::validate(format!(
                    "Remove restricted_feature '{}' as manifest does not contain facet '{}'",
                    Feature::AllowNonHermeticPackages,
                    TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY
                )));
            }
        }
        Ok(())
    }

    fn validate_child(&mut self, child: &'a Child) -> Result<(), Error> {
        if let Some(resource) = child.url.resource() {
            if resource.ends_with(".cml") {
                return Err(Error::validate(format!(
                    "child URL ends in .cml instead of .cm, \
which is almost certainly a mistake: {}",
                    child.url
                )));
            }
        }

        if let Some(environment_ref) = &child.environment {
            match environment_ref {
                EnvironmentRef::Named(environment_name) => {
                    if !self.all_environment_names.contains(&environment_name) {
                        return Err(Error::validate(format!(
                            "\"{}\" does not appear in \"environments\"",
                            &environment_name
                        )));
                    }
                    let source = DependencyNode::Named(&environment_name);
                    let target = DependencyNode::Named(&child.name);
                    self.add_strong_dep(source, target);
                }
            }
        }
        Ok(())
    }

    fn validate_collection(&mut self, collection: &'a Collection) -> Result<(), Error> {
        if collection.allow_long_names.is_some() {
            self.features.check(Feature::AllowLongNames)?;
        }
        if let Some(environment_ref) = &collection.environment {
            match environment_ref {
                EnvironmentRef::Named(environment_name) => {
                    if !self.all_environment_names.contains(&environment_name) {
                        return Err(Error::validate(format!(
                            "\"{}\" does not appear in \"environments\"",
                            &environment_name
                        )));
                    }
                    let source = DependencyNode::Named(&environment_name);
                    let target = DependencyNode::Named(&collection.name);
                    self.add_strong_dep(source, target);
                }
            }
        }
        Ok(())
    }

    fn validate_capability(
        &mut self,
        capability: &'a Capability,
        used_ids: &mut HashMap<String, CapabilityId<'a>>,
    ) -> Result<(), Error> {
        if capability.directory.is_some() && capability.path.is_none() {
            return Err(Error::validate("\"path\" should be present with \"directory\""));
        }
        if capability.directory.is_some() && capability.rights.is_none() {
            return Err(Error::validate("\"rights\" should be present with \"directory\""));
        }
        if let Some(name) = capability.storage.as_ref() {
            if capability.from.is_none() {
                return Err(Error::validate("\"from\" should be present with \"storage\""));
            }
            if capability.path.is_some() {
                return Err(Error::validate(
                    "\"path\" can not be present with \"storage\", use \"backing_dir\"",
                ));
            }
            if capability.backing_dir.is_none() {
                return Err(Error::validate("\"backing_dir\" should be present with \"storage\""));
            }
            if capability.storage_id.is_none() {
                return Err(Error::validate("\"storage_id\" should be present with \"storage\""));
            }

            // The storage capability depends on its backing dir.
            let target = DependencyNode::Named(name);
            let source = capability.from.as_ref().unwrap();
            let names = vec![capability.backing_dir.as_ref().unwrap()];
            for source in self.expand_source_dependencies(names, &source.into()) {
                self.add_strong_dep(source, target);
            }
        }
        if capability.runner.is_some() && capability.from.is_some() {
            return Err(Error::validate("\"from\" should not be present with \"runner\""));
        }
        if capability.runner.is_some() && capability.path.is_none() {
            return Err(Error::validate("\"path\" should be present with \"runner\""));
        }
        if capability.resolver.is_some() && capability.from.is_some() {
            return Err(Error::validate("\"from\" should not be present with \"resolver\""));
        }
        if capability.resolver.is_some() && capability.path.is_none() {
            return Err(Error::validate("\"path\" should be present with \"resolver\""));
        }

        if let Some(name) = capability.dictionary.as_ref() {
            if capability.path.is_some() {
                self.features.check(Feature::DynamicDictionaries)?;
                // If `path` is set that means the dictionary is provided by the program,
                // which implies a dependency from `self` to the dictionary declaration.
                let target = DependencyNode::Named(name);
                self.add_strong_dep(DependencyNode::Self_, target);
            }
        }
        if capability.delivery.is_some() {
            self.features.check(Feature::DeliveryType)?;
        }
        if let Some(from) = capability.from.as_ref() {
            self.validate_component_child_ref("\"capabilities\" source", &AnyRef::from(from))?;
        }

        // Disallow multiple capability ids of the same name.
        let capability_ids = CapabilityId::from_capability(capability)?;
        for capability_id in capability_ids {
            if used_ids.insert(capability_id.to_string(), capability_id.clone()).is_some() {
                return Err(Error::validate(format!(
                    "\"{}\" is a duplicate \"capability\" name",
                    capability_id,
                )));
            }
        }

        Ok(())
    }

    fn validate_use(
        &mut self,
        use_: &'a Use,
        used_ids: &mut HashMap<String, CapabilityId<'a>>,
    ) -> Result<(), Error> {
        use_.capability_type()?;
        for checker in [
            self.service_from_self_checker(use_),
            self.protocol_from_self_checker(use_),
            self.directory_from_self_checker(use_),
            self.config_from_self_checker(use_),
        ] {
            checker.validate("used")?;
        }

        if use_.from == Some(UseFromRef::Debug) && use_.protocol.is_none() {
            return Err(Error::validate("only \"protocol\" supports source from \"debug\""));
        }
        if use_.event_stream.is_some() && use_.availability.is_some() {
            return Err(Error::validate("\"availability\" cannot be used with \"event_stream\""));
        }
        if use_.event_stream.is_none() && use_.filter.is_some() {
            return Err(Error::validate("\"filter\" can only be used with \"event_stream\""));
        }
        if use_.storage.is_some() && use_.from.is_some() {
            return Err(Error::validate("\"from\" cannot be used with \"storage\""));
        }
        if use_.runner.is_some() && use_.availability.is_some() {
            return Err(Error::validate("\"availability\" cannot be used with \"runner\""));
        }
        if use_.from == Some(UseFromRef::Self_) && use_.event_stream.is_some() {
            return Err(Error::validate("\"from: self\" cannot be used with \"event_stream\""));
        }
        if use_.from == Some(UseFromRef::Self_) && use_.runner.is_some() {
            return Err(Error::validate("\"from: self\" cannot be used with \"runner\""));
        }
        if use_.availability == Some(Availability::SameAsTarget) {
            return Err(Error::validate(
                "\"availability: same_as_target\" cannot be used with use declarations",
            ));
        }
        if let Some(UseFromRef::Dictionary(_)) = use_.from.as_ref() {
            if use_.storage.is_some() {
                return Err(Error::validate(
                    "Dictionaries do not support \"storage\" capabilities",
                ));
            }
            if use_.event_stream.is_some() {
                return Err(Error::validate(
                    "Dictionaries do not support \"event_stream\" capabilities",
                ));
            }
        }
        if let Some(config) = use_.config.as_ref() {
            if use_.key == None {
                return Err(Error::validate(format!("Config '{}' missing field 'key'", config)));
            }
            let _ = use_config_to_value_type(use_)?;
            let availability = use_.availability.unwrap_or(Availability::Required);
            if availability == Availability::Required && use_.config_default.is_some() {
                return Err(Error::validate(format!(
                    "Config '{}' is required and has a default value",
                    config
                )));
            }
        }

        if let Some(source) = use_.from.as_ref() {
            for source in self.expand_source_dependencies(use_.names(), &source.into()) {
                let target = DependencyNode::Self_;
                if use_.dependency.as_ref().unwrap_or(&DependencyType::Strong)
                    == &DependencyType::Strong
                {
                    self.add_strong_dep(source, target);
                }
            }
        }

        // Disallow multiple capability ids of the same name.
        let capability_ids = CapabilityId::from_use(use_)?;
        for capability_id in capability_ids {
            if used_ids.insert(capability_id.to_string(), capability_id.clone()).is_some() {
                return Err(Error::validate(format!(
                    "\"{}\" is a duplicate \"use\" target {}",
                    capability_id,
                    capability_id.type_str()
                )));
            }
            let dir = capability_id.get_dir_path();

            // Capability paths must not conflict with `/pkg`, or namespace generation might fail
            let pkg_path = cm_types::NamespacePath::new("/pkg").unwrap();
            if let Some(ref dir) = dir {
                if dir.has_prefix(&pkg_path) {
                    return Err(Error::validate(format!(
                        "{} \"{}\" conflicts with the protected path \"/pkg\", please use this capability with a different path",
                        capability_id.type_str(), capability_id,
                    )));
                }
            }

            // Validate that paths-based capabilities (service, directory, protocol)
            // are not prefixes of each other.
            for (_, used_id) in used_ids.iter() {
                if capability_id == *used_id {
                    continue;
                }
                let Some(ref dir) = dir else {
                    continue;
                };
                let Some(used_dir) = used_id.get_dir_path() else {
                    continue;
                };

                if match (used_id, &capability_id) {
                    // Directories and storage can't be the same or partially overlap.
                    (CapabilityId::UsedDirectory(_), CapabilityId::UsedStorage(_))
                    | (CapabilityId::UsedStorage(_), CapabilityId::UsedDirectory(_))
                    | (CapabilityId::UsedDirectory(_), CapabilityId::UsedDirectory(_))
                    | (CapabilityId::UsedStorage(_), CapabilityId::UsedStorage(_)) => {
                        dir.has_prefix(&used_dir) || used_dir.has_prefix(&dir)
                    }

                    // Protocols and services can't overlap with directories or storage.
                    (CapabilityId::UsedDirectory(_), _)
                    | (CapabilityId::UsedStorage(_), _)
                    | (_, CapabilityId::UsedDirectory(_))
                    | (_, CapabilityId::UsedStorage(_)) => {
                        dir.has_prefix(&used_dir) || used_dir.has_prefix(&dir)
                    }

                    // Protocols and services containing directories may be same, but
                    // partial overlap is disallowed.
                    (_, _) => {
                        *dir != used_dir && (dir.has_prefix(&used_dir) || used_dir.has_prefix(&dir))
                    }
                } {
                    return Err(Error::validate(format!(
                        "{} \"{}\" is a prefix of \"use\" target {} \"{}\"",
                        used_id.type_str(),
                        used_id,
                        capability_id.type_str(),
                        capability_id,
                    )));
                }
            }
        }

        if let Some(_) = use_.directory.as_ref() {
            // All directory "use" expressions must have directory rights.
            match &use_.rights {
                Some(rights) => self.validate_directory_rights(&rights)?,
                None => {
                    return Err(Error::validate("This use statement requires a `rights` field. Refer to: https://fuchsia.dev/go/components/directory#consumer."))
                }
            };
        }

        match (&use_.from, &use_.dependency) {
            (Some(UseFromRef::Named(name)), _) => {
                self.validate_component_child_or_capability_ref(
                    "\"use\" source",
                    &AnyRef::Named(name),
                )?;
            }
            (_, Some(DependencyType::Weak)) => {
                return Err(Error::validate(format!(
                    "Only `use` from children can have dependency: \"weak\""
                )));
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_expose(
        &self,
        expose: &'a Expose,
        used_ids: &mut HashMap<String, CapabilityId<'a>>,
    ) -> Result<(), Error> {
        expose.capability_type()?;
        for checker in [
            self.service_from_self_checker(expose),
            self.protocol_from_self_checker(expose),
            self.directory_from_self_checker(expose),
            self.runner_from_self_checker(expose),
            self.resolver_from_self_checker(expose),
            self.dictionary_from_self_checker(expose),
            self.config_from_self_checker(expose),
        ] {
            checker.validate("exposed")?;
        }

        // Ensure that if the expose target is framework, the source target is self always.
        if expose.to == Some(ExposeToRef::Framework) {
            match &expose.from {
                OneOrMany::One(ExposeFromRef::Self_) => {}
                OneOrMany::Many(vec) if vec.iter().all(|from| *from == ExposeFromRef::Self_) => {}
                _ => {
                    return Err(Error::validate("Expose to framework can only be done from self."))
                }
            }
        }

        // Ensure directory rights are valid.
        if let Some(_) = expose.directory.as_ref() {
            if expose.from.iter().any(|r| *r == ExposeFromRef::Self_) || expose.rights.is_some() {
                if let Some(rights) = expose.rights.as_ref() {
                    self.validate_directory_rights(&rights)?;
                }
            }

            // Exposing a subdirectory makes sense for routing but when exposing to framework,
            // the subdir should be exposed directly.
            if expose.to == Some(ExposeToRef::Framework) {
                if expose.subdir.is_some() {
                    return Err(Error::validate(
                        "`subdir` is not supported for expose to framework. Directly expose the subdirectory instead."
                    ));
                }
            }
        }

        if let Some(event_stream) = &expose.event_stream {
            if event_stream.iter().len() > 1 && expose.r#as.is_some() {
                return Err(Error::validate(format!(
                    "as cannot be used with multiple event streams"
                )));
            }
            if let Some(ExposeToRef::Framework) = &expose.to {
                return Err(Error::validate(format!("cannot expose an event_stream to framework")));
            }
            for from in expose.from.iter() {
                if from == &ExposeFromRef::Self_ {
                    return Err(Error::validate(format!("Cannot expose event_streams from self")));
                }
            }
            if let Some(scopes) = &expose.scope {
                for scope in scopes {
                    match scope {
                        EventScope::Named(name) => {
                            if !self.all_children.contains_key(name)
                                && !self.all_collections.contains(name)
                            {
                                return Err(Error::validate(format!("event_stream scope {} did not match a component or collection in this .cml file.", name.as_str())));
                            }
                        }
                    }
                }
            }
        }

        for ref_ in expose.from.iter() {
            if let ExposeFromRef::Dictionary(d) = ref_ {
                if expose.event_stream.is_some() {
                    return Err(Error::validate(
                        "Dictionaries do not support \"event_stream\" capabilities",
                    ));
                }
                match &d.root {
                    RootDictionaryRef::Self_ | RootDictionaryRef::Named(_) => {}
                    RootDictionaryRef::Parent => {
                        return Err(Error::validate(
                            "`expose` dictionary path must begin with `self` or `#<child-name>`",
                        ));
                    }
                }
            }
        }

        // Ensure we haven't already exposed an entity of the same name.
        let capability_ids = CapabilityId::from_offer_expose(expose)?;
        for capability_id in capability_ids {
            if used_ids.insert(capability_id.to_string(), capability_id.clone()).is_some() {
                if let CapabilityId::Service(_) = capability_id {
                    // Services may have duplicates (aggregation).
                } else {
                    return Err(Error::validate(format!(
                        "\"{}\" is a duplicate \"expose\" target capability for \"{}\"",
                        capability_id,
                        expose.to.as_ref().unwrap_or(&ExposeToRef::Parent)
                    )));
                }
            }
        }

        // Validate `from` (done last because this validation depends on the capability type, which
        // must be validated first)
        self.validate_from_clause(
            "expose",
            expose,
            &expose.source_availability,
            &expose.availability,
        )?;

        Ok(())
    }

    fn validate_offer(
        &mut self,
        offer: &'a Offer,
        used_ids: &mut HashMap<Name, HashMap<String, CapabilityId<'a>>>,
        protocols_offered_to_all: &[&'a Offer],
    ) -> Result<(), Error> {
        offer.capability_type()?;
        for checker in [
            self.service_from_self_checker(offer),
            self.protocol_from_self_checker(offer),
            self.directory_from_self_checker(offer),
            self.storage_from_self_checker(offer),
            self.runner_from_self_checker(offer),
            self.resolver_from_self_checker(offer),
            self.dictionary_from_self_checker(offer),
            self.config_from_self_checker(offer),
        ] {
            checker.validate("offered")?;
        }

        if let Some(stream) = offer.event_stream.as_ref() {
            if stream.iter().len() > 1 && offer.r#as.is_some() {
                return Err(Error::validate(format!("as cannot be used with multiple events")));
            }
            for from in &offer.from {
                match from {
                    OfferFromRef::Self_ => {
                        return Err(Error::validate(format!(
                            "cannot offer an event_stream from self"
                        )));
                    }
                    _ => {}
                }
            }
        }

        // Ensure directory rights are valid.
        if let Some(_) = offer.directory.as_ref() {
            if offer.from.iter().any(|r| *r == OfferFromRef::Self_) || offer.rights.is_some() {
                if let Some(rights) = offer.rights.as_ref() {
                    self.validate_directory_rights(&rights)?;
                }
            }
        }

        if let Some(storage) = offer.storage.as_ref() {
            for storage in storage {
                if offer.from.iter().any(|r| r.is_named()) {
                    return Err(Error::validate(format!(
                    "Storage \"{}\" is offered from a child, but storage capabilities cannot be exposed", storage)));
                }
            }
        }

        for ref_ in offer.from.iter() {
            if let OfferFromRef::Dictionary(d) = ref_ {
                match &d.root {
                    RootDictionaryRef::Self_
                    | RootDictionaryRef::Named(_)
                    | RootDictionaryRef::Parent => {}
                }

                if offer.storage.is_some() {
                    return Err(Error::validate(
                        "Dictionaries do not support \"storage\" capabilities",
                    ));
                }
                if offer.event_stream.is_some() {
                    return Err(Error::validate(
                        "Dictionaries do not support \"event_stream\" capabilities",
                    ));
                }
            }
        }

        // Ensure that dependency is set for the right capabilities.
        if !offer_can_have_dependency(offer) && offer.dependency.is_some() {
            return Err(Error::validate(
                "Dependency can only be provided for protocol, directory, and service capabilities",
            ));
        }

        // Validate every target of this offer.
        let target_cap_ids = CapabilityId::from_offer_expose(offer)?;
        for to in &offer.to {
            // Ensure the "to" value is a child, collection, or dictionary capability.
            let to_target = match to {
                OfferToRef::All => continue,
                OfferToRef::Named(ref to_target) => {
                    // Verify that only a legal set of offers-to-all are made, including that any
                    // offer to all duplicated as an offer to a specific component are exactly the same
                    for offer_to_all in protocols_offered_to_all {
                        offer_to_all_would_duplicate(offer_to_all, offer, to_target)?;
                    }

                    // Check that any referenced child actually exists.
                    if self.all_children.contains_key(to_target)
                        || self.all_collections.contains(to_target)
                    {
                        // Allowed.
                    } else {
                        if let OneOrMany::One(from) = &offer.from {
                            return Err(Error::validate(format!(
                                "\"{to}\" is an \"offer\" target from \"{from}\" but it does \
                                not appear in \"children\" or \"collections\"",
                            )));
                        } else {
                            return Err(Error::validate(format!(
                                "\"{to}\" is an \"offer\" target but it does not appear in \
                                \"children\" or \"collections\"",
                            )));
                        }
                    }

                    // Ensure we are not offering a capability back to its source.
                    if let Some(storage) = offer.storage.as_ref() {
                        for storage in storage {
                            // Storage can only have a single `from` clause and this has been
                            // verified.
                            if let OneOrMany::One(OfferFromRef::Self_) = &offer.from {
                                if let Some(CapabilityFromRef::Named(source)) =
                                    self.all_storages.get(storage)
                                {
                                    if to_target == source {
                                        return Err(Error::validate(format!(
                                            "Storage offer target \"{}\" is same as source",
                                            to
                                        )));
                                    }
                                }
                            }
                        }
                    } else {
                        for reference in offer.from.iter() {
                            // Weak offers from a child to itself are acceptable.
                            if offer_dependency(offer) == DependencyType::Weak {
                                continue;
                            }
                            match reference {
                                OfferFromRef::Named(name) if name == to_target => {
                                    return Err(Error::validate(format!(
                                        "Offer target \"{}\" is same as source",
                                        to
                                    )));
                                }
                                _ => {}
                            }
                        }
                    }
                    to_target
                }
                OfferToRef::OwnDictionary(ref to_target) => {
                    if let Ok(capability_ids) = CapabilityId::from_offer_expose(offer) {
                        for id in capability_ids {
                            match &id {
                                CapabilityId::Protocol(_)
                                | CapabilityId::Dictionary(_)
                                | CapabilityId::Runner(_)
                                | CapabilityId::Resolver(_)
                                | CapabilityId::Configuration(_) => {}
                                CapabilityId::Directory(_)
                                | CapabilityId::Service(_)
                                | CapabilityId::Storage(_)
                                | CapabilityId::EventStream(_) => {
                                    let type_name = id.type_str();
                                    return Err(Error::validate(format!(
                                        "\"offer\" to dictionary \"{to}\" for \"{type_name}\" but \
                                        dictionaries do not support this type yet."
                                    )));
                                }
                                CapabilityId::UsedService(_)
                                | CapabilityId::UsedProtocol(_)
                                | CapabilityId::UsedDirectory(_)
                                | CapabilityId::UsedStorage(_)
                                | CapabilityId::UsedEventStream(_)
                                | CapabilityId::UsedRunner(_)
                                | CapabilityId::UsedConfiguration(_) => {
                                    unreachable!("this is not a use")
                                }
                            }
                        }
                    }
                    // Check that any referenced child actually exists.
                    let Some(d) = self.all_dictionaries.get(&to_target) else {
                        return Err(Error::validate(format!(
                            "\"offer\" has dictionary target \"{to}\" but \"{to_target}\" \
                                is not a dictionary capability defined by this component"
                        )));
                    };
                    if d.path.is_some() {
                        return Err(Error::validate(format!(
                            "\"offer\" has dictionary target \"{to}\" but \"{to_target}\" \
                            sets \"path\". Therefore, it is a dynamic dictionary that \
                            does not allow offers into it."
                        )));
                    }
                    to_target
                }
            };

            // Ensure that a target is not offered more than once.
            let ids_for_entity = used_ids.entry(to_target.clone()).or_insert(HashMap::new());
            for target_cap_id in &target_cap_ids {
                if ids_for_entity.insert(target_cap_id.to_string(), target_cap_id.clone()).is_some()
                {
                    if let CapabilityId::Service(_) = target_cap_id {
                        // Services may have duplicates (aggregation).
                    } else {
                        return Err(Error::validate(format!(
                            "\"{}\" is a duplicate \"offer\" target capability for \"{}\"",
                            target_cap_id, to
                        )));
                    }
                }
            }

            // Collect strong dependencies. We'll check for dependency cycles after all offer
            // declarations are validated.
            for from in offer.from.iter() {
                if offer_dependency(offer) == DependencyType::Strong {
                    for source in self.expand_source_dependencies(offer.names(), &from.into()) {
                        let target = DependencyNode::offer_to_ref(to);
                        self.add_strong_dep(source, target);
                    }
                }
            }
        }

        // Validate `from` (done last because this validation depends on the capability type, which
        // must be validated first)
        self.validate_from_clause("offer", offer, &offer.source_availability, &offer.availability)?;

        Ok(())
    }

    fn validate_required_offer_decls(&self) -> Result<(), Error> {
        let children_stub = Vec::new();
        let children = self.document.children.as_ref().unwrap_or(&children_stub);
        let collections_stub = Vec::new();
        let collections = self.document.collections.as_ref().unwrap_or(&collections_stub);
        let offers_stub = Vec::new();
        let offers = self.document.offer.as_ref().unwrap_or(&offers_stub);
        let must_offer_protocol_stub = Vec::new();
        let disable_stub = Disable::default();
        let disabled_required_offers = self
            .document
            .disable
            .as_ref()
            .unwrap_or(&disable_stub)
            .must_offer_protocol
            .as_ref()
            .unwrap_or(&must_offer_protocol_stub);

        for required_offer in self.protocol_requirements.must_offer {
            if disabled_required_offers.iter().any(|offer| required_offer == offer) {
                continue;
            }

            // for each child, check if any offer is:
            //   1) Targeting this child (or all)
            //   AND
            //   2) Offering the current required protocol
            for child in children.iter() {
                if !offers.iter().any(|offer| {
                    let names_this_child = offer.to.iter().any(|target| match target {
                        OfferToRef::Named(ref name) => name == &child.name,
                        OfferToRef::All => true,
                        OfferToRef::OwnDictionary(_) => false,
                    });

                    let names_this_capability = match offer.protocol.as_ref() {
                        Some(protocol) => {
                            protocol.iter().any(|proto| proto.as_str() == required_offer)
                        }
                        None => false,
                    };
                    names_this_child && names_this_capability
                }) {
                    return Err(Error::validate(format!(
                        r#"Protocol "{}" is not offered to child component "{}" but it is a required offer"#,
                        required_offer, child.name
                    )));
                }
            }

            for collection in collections.iter() {
                if !offers.iter().any(|offer| {
                    let names_this_collection = offer.to.iter().any(|target| match target {
                        OfferToRef::Named(ref name) => name == &collection.name,
                        OfferToRef::All => true,
                        OfferToRef::OwnDictionary(_) => false,
                    });

                    let names_this_capability = match offer.protocol.as_ref() {
                        Some(protocol) => {
                            protocol.iter().any(|proto| proto.as_str() == required_offer)
                        }
                        None => false,
                    };
                    names_this_collection && names_this_capability
                }) {
                    return Err(Error::validate(format!(
                        r#"Protocol "{}" is not offered to collection "{}" but it is a required offer"#,
                        required_offer, collection.name
                    )));
                }
            }
        }

        Ok(())
    }

    fn validate_required_use_decls(&self) -> Result<(), Error> {
        let use_decls_stub = Vec::new();
        let use_decls = self.document.r#use.as_ref().unwrap_or(&use_decls_stub);
        let disable_stub = Disable::default();
        let disabled_must_use_protocol_stub = Vec::new();
        let disabled_required_use_decls = self
            .document
            .disable
            .as_ref()
            .unwrap_or(&disable_stub)
            .must_use_protocol
            .as_ref()
            .unwrap_or(&disabled_must_use_protocol_stub);

        for required_usage in self.protocol_requirements.must_use {
            if disabled_required_use_decls.iter().any(|usage| required_usage == usage) {
                continue;
            }

            if !use_decls.iter().any(|usage| match usage.protocol.as_ref() {
                None => false,
                Some(protocol) => {
                    protocol.iter().any(|protocol_name| protocol_name.as_str() == required_usage)
                }
            }) {
                return Err(Error::validate(format!(
                    r#"Protocol "{}" is not used by a component but is required by all"#,
                    required_usage,
                )));
            }
        }

        Ok(())
    }

    fn expand_source_dependencies(
        &self,
        names: Vec<&'a Name>,
        source: &AnyRef<'a>,
    ) -> Vec<DependencyNode<'a>> {
        let mut sources = vec![];
        match source {
            AnyRef::Self_ => {
                let mut has_named = false;
                for name in names {
                    if self.all_dictionaries.contains_key(name)
                        || self.all_storages.contains_key(name)
                    {
                        sources.push(DependencyNode::Named(name));
                        has_named = true;
                    }
                }
                if !has_named {
                    sources.push(DependencyNode::Self_);
                }
            }
            AnyRef::Dictionary(d) => {
                let mut has_named = false;
                match d.root {
                    RootDictionaryRef::Self_ => {
                        let dictionary_name = d.path.iter_segments().next().unwrap();
                        if self.all_dictionaries.contains_key(dictionary_name) {
                            // This should be true, if the cml didn't contain a syntax error that
                            // omitted the definition for `dictionary_name`.
                            sources.push(DependencyNode::Named(dictionary_name));
                            has_named = true;
                        }
                    }
                    _ => {}
                }
                if !has_named {
                    if let Some(s) = DependencyNode::from_dictionary_ref(d) {
                        sources.push(s);
                    }
                }
            }
            AnyRef::Named(n) => {
                sources.push(DependencyNode::Named(n));
            }
            AnyRef::Parent | AnyRef::Void | AnyRef::Framework | AnyRef::Debug => {}
            AnyRef::OwnDictionary(_) => unreachable!("can't be a source"),
        }
        sources
    }

    /// Adds a strong dependency between two nodes in the dependency graph between `source` and
    /// `target`.
    ///
    /// `name` is the name of the capability being routed (if applicable).
    fn add_strong_dep(&mut self, source: DependencyNode<'a>, target: DependencyNode<'a>) {
        match (source, target) {
            (DependencyNode::Self_, DependencyNode::Self_) => {
                // `self` dependencies (e.g. `use from self`) are allowed.
            }
            (source, target) => {
                self.strong_dependencies.add_edge(source, target);
            }
        }
    }

    /// Validates that the from clause:
    ///
    /// - is applicable to the capability type,
    /// - does not contain duplicates,
    /// - references names that exist.
    /// - has availability "optional" if the source is "void"
    ///
    /// `verb` is used in any error messages and is expected to be "offer", "expose", etc.
    fn validate_from_clause<T>(
        &self,
        verb: &str,
        cap: &T,
        source_availability: &Option<SourceAvailability>,
        availability: &Option<Availability>,
    ) -> Result<(), Error>
    where
        T: CapabilityClause + FromClause,
    {
        let from = cap.from_();
        if cap.service().is_none() && from.is_many() {
            return Err(Error::validate(format!(
                "\"{}\" capabilities cannot have multiple \"from\" clauses",
                cap.capability_type().unwrap()
            )));
        }

        if from.is_many() {
            ensure_no_duplicate_values(&cap.from_())?;
        }

        let reference_description = format!("\"{}\" source", verb);
        for from_clause in from {
            // If this is a protocol, it could reference either a child or a storage capability
            // (for the storage admin protocol).
            let ref_validity_res = if cap.protocol().is_some() {
                self.validate_component_child_or_capability_ref(
                    &reference_description,
                    &from_clause,
                )
            } else if cap.service().is_some() {
                // Services can also be sourced from collections.
                self.validate_component_child_or_collection_ref(
                    &reference_description,
                    &from_clause,
                )
            } else {
                self.validate_component_child_ref(&reference_description, &from_clause)
            };

            match ref_validity_res {
                Ok(()) if from_clause == AnyRef::Void => {
                    // The source is valid and void
                    if availability != &Some(Availability::Optional) {
                        return Err(Error::validate(format!(
                            "capabilities with a source of \"void\" must have an availability of \"optional\", capabilities: \"{}\", from: \"{}\"",
                            cap.names().iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", "),
                            cap.from_(),
                        )));
                    }
                }
                Ok(()) => {
                    // The source is valid and not void.
                }
                Err(_) if source_availability == &Some(SourceAvailability::Unknown) => {
                    // The source is invalid, and will be rewritten to void
                    if availability != &Some(Availability::Optional) && availability != &None {
                        return Err(Error::validate(format!(
                            "capabilities with an intentionally missing source must have an availability that is either unset or \"optional\", capabilities: \"{}\", from: \"{}\"",
                            cap.names().iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", "),
                            cap.from_(),
                        )));
                    }
                }
                Err(e) => {
                    // The source is invalid, but we're expecting it to be valid.
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Validates that the given component exists.
    ///
    /// - `reference_description` is a human-readable description of the reference used in error
    ///   message, such as `"offer" source`.
    /// - `component_ref` is a reference to a component. If the reference is a named child, we
    ///   ensure that the child component exists.
    fn validate_component_child_ref(
        &self,
        reference_description: &str,
        component_ref: &AnyRef<'_>,
    ) -> Result<(), Error> {
        match component_ref {
            AnyRef::Named(name) => {
                // Ensure we have a child defined by that name.
                if !self.all_children.contains_key(name) {
                    return Err(Error::validate(format!(
                        "{} \"{}\" does not appear in \"children\"",
                        reference_description, component_ref
                    )));
                }
                Ok(())
            }
            // We don't attempt to validate other reference types.
            _ => Ok(()),
        }
    }

    /// Validates that the given component/collection exists.
    ///
    /// - `reference_description` is a human-readable description of the reference used in error
    ///   message, such as `"offer" source`.
    /// - `component_ref` is a reference to a component/collection. If the reference is a named
    ///   child or collection, we ensure that the child component/collection exists.
    fn validate_component_child_or_collection_ref(
        &self,
        reference_description: &str,
        component_ref: &AnyRef<'_>,
    ) -> Result<(), Error> {
        match component_ref {
            AnyRef::Named(name) => {
                // Ensure we have a child defined by that name.
                if !self.all_children.contains_key(name) && !self.all_collections.contains(name) {
                    return Err(Error::validate(format!(
                        "{} \"{}\" does not appear in \"children\" or \"collections\"",
                        reference_description, component_ref
                    )));
                }
                Ok(())
            }
            // We don't attempt to validate other reference types.
            _ => Ok(()),
        }
    }

    /// Validates that the given capability exists.
    ///
    /// - `reference_description` is a human-readable description of the reference used in error
    ///   message, such as `"offer" source`.
    /// - `capability_ref` is a reference to a capability. If the reference is a named capability,
    ///   we ensure that the capability exists.
    fn validate_component_capability_ref(
        &self,
        reference_description: &str,
        capability_ref: &AnyRef<'_>,
    ) -> Result<(), Error> {
        match capability_ref {
            AnyRef::Named(name) => {
                if !self.all_capability_names.contains(name) {
                    return Err(Error::validate(format!(
                        "{} \"{}\" does not appear in \"capabilities\"",
                        reference_description, capability_ref
                    )));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Validates that the given child component or capability exists.
    ///
    /// - `reference_description` is a human-readable description of the reference used in error
    ///   message, such as `"offer" source`.
    /// - `ref_` is a reference to a child component or capability. If the reference contains a
    ///   name, we ensure that a child component or a capability with the name exists.
    fn validate_component_child_or_capability_ref(
        &self,
        reference_description: &str,
        ref_: &AnyRef<'_>,
    ) -> Result<(), Error> {
        if self.validate_component_child_ref(reference_description, ref_).is_err()
            && self.validate_component_capability_ref(reference_description, ref_).is_err()
        {
            return Err(Error::validate(format!(
                "{} \"{}\" does not appear in \"children\" or \"capabilities\"",
                reference_description, ref_
            )));
        }
        Ok(())
    }

    /// Validates that directory rights for all route types are valid, i.e that it does not
    /// contain duplicate rights.
    fn validate_directory_rights(&self, rights_clause: &Rights) -> Result<(), Error> {
        let mut rights = HashSet::new();
        for right_token in rights_clause.0.iter() {
            for right in right_token.expand() {
                if !rights.insert(right) {
                    return Err(Error::validate(format!(
                        "\"{}\" is duplicated in the rights clause.",
                        right_token
                    )));
                }
            }
        }
        Ok(())
    }

    /// Ensure we don't have a component with a "program" block which fails to specify a runner.
    /// This should only be called if the manifest doesn't "use" a runner.
    fn validate_runner_specified(&self, program: Option<&Program>) -> Result<(), Error> {
        match program {
            Some(program) => match program.runner {
                Some(_) => Ok(()),
                None => {
                    return Err(Error::validate(
                        "Component has a `program` block defined, but doesn't specify a `runner`. \
                        Components need to use a runner to actually execute code.",
                    ));
                }
            },
            None => Ok(()),
        }
    }

    /// Ensure we don't have a component with a "program" block which fails to specify a runner.
    /// This should only be called if the manifest "use"s a runner.
    fn validate_runner_not_specified(&self, program: Option<&Program>) -> Result<(), Error> {
        match program {
            Some(program) => match program.runner {
                Some(_) => {
                    // Use/runner always conflicts with program/runner, because use/runner
                    // can't be from environment in CML.
                    return Err(Error::validate(
                        "Component has conflicting runners in `program` block and `use` block.",
                    ));
                }
                None => Ok(()),
            },
            None => Ok(()),
        }
    }

    fn validate_config(
        &self,
        fields: &Option<BTreeMap<ConfigKey, ConfigValueType>>,
    ) -> Result<(), Error> {
        // If we `use` a config capability optionally without a default then it has to exist in the `config` block.
        // Collect the names of the keys here.
        let optional_use_keys: BTreeMap<ConfigKey, ConfigValueType> = self
            .document
            .r#use
            .iter()
            .flatten()
            .map(|u| {
                if u.config == None {
                    return None;
                }
                if u.availability == Some(Availability::Required) || u.availability == None {
                    return None;
                }
                if let Some(_) = u.config_default.as_ref() {
                    return None;
                }
                let key = ConfigKey(u.key.clone().expect("key should be set").into());
                let value = use_config_to_value_type(u).expect("config type should be valid");
                Some((key, value))
            })
            .flatten()
            .collect();

        let Some(fields) = fields else {
            if !optional_use_keys.is_empty() {
                return Err(Error::validate(
                    "Optionally using a config capability without a default requires a matching 'config' section.",
                ));
            }
            return Ok(());
        };

        if fields.is_empty() {
            return Err(Error::validate("'config' section is empty"));
        }

        for (key, value) in optional_use_keys {
            if !fields.contains_key(&key) {
                return Err(Error::validate(format!(
                    "'config' section must contain key for optional use '{}'",
                    key
                )));
            }
            if fields.get(&key) != Some(&value) {
                return Err(Error::validate(format!(
                    "Use and config block differ on type for key '{}'",
                    key
                )));
            }
        }

        Ok(())
    }

    fn validate_environment(&mut self, environment: &'a Environment) -> Result<(), Error> {
        match &environment.extends {
            Some(EnvironmentExtends::None) => {
                if environment.stop_timeout_ms.is_none() {
                    return Err(Error::validate(
                        "'__stop_timeout_ms' must be provided if the environment does not extend \
                        another environment",
                    ));
                }
            }
            Some(EnvironmentExtends::Realm) | None => {}
        }

        if let Some(runners) = &environment.runners {
            let mut used_names = HashMap::new();
            for registration in runners {
                // Validate that this name is not already used.
                let name = registration.r#as.as_ref().unwrap_or(&registration.runner);
                if let Some(previous_runner) = used_names.insert(name, &registration.runner) {
                    return Err(Error::validate(format!(
                        "Duplicate runners registered under name \"{}\": \"{}\" and \"{}\".",
                        name, &registration.runner, previous_runner
                    )));
                }

                // Ensure that the environment is defined in `runners` if it comes from `self`.
                if registration.from == RegistrationRef::Self_
                    && !self.all_runners.contains(&registration.runner)
                {
                    return Err(Error::validate(format!(
                        "Runner \"{}\" registered in environment is not in \"runners\"",
                        &registration.runner,
                    )));
                }

                self.validate_component_child_ref(
                    &format!("\"{}\" runner source", &registration.runner),
                    &AnyRef::from(&registration.from),
                )?;

                // Ensure there are no cycles, such as a resolver in an environment being assigned
                // to a child which the resolver depends on.
                if let Some(source) = DependencyNode::registration_ref(&registration.from) {
                    let target = DependencyNode::Named(&environment.name);
                    self.add_strong_dep(source, target);
                }
            }
        }

        if let Some(resolvers) = &environment.resolvers {
            let mut used_schemes = HashMap::new();
            for registration in resolvers {
                // Validate that the scheme is not already used.
                if let Some(previous_resolver) =
                    used_schemes.insert(&registration.scheme, &registration.resolver)
                {
                    return Err(Error::validate(format!(
                        "scheme \"{}\" for resolver \"{}\" is already registered; \
                        previously registered to resolver \"{}\".",
                        &registration.scheme, &registration.resolver, previous_resolver
                    )));
                }

                self.validate_component_child_ref(
                    &format!("\"{}\" resolver source", &registration.resolver),
                    &AnyRef::from(&registration.from),
                )?;
                // Ensure there are no cycles, such as a resolver in an environment being assigned
                // to a child which the resolver depends on.
                if let Some(source) = DependencyNode::registration_ref(&registration.from) {
                    let target = DependencyNode::Named(&environment.name);
                    self.add_strong_dep(source, target);
                }
            }
        }

        if let Some(debug_capabilities) = &environment.debug {
            for debug in debug_capabilities {
                self.protocol_from_self_checker(debug).validate("registered as debug")?;
                self.validate_from_clause("debug", debug, &None, &None)?;
                // Ensure there are no cycles, such as a debug capability in an environment being
                // assigned to the child which is providing the capability.
                for source in self.expand_source_dependencies(debug.names(), &(&debug.from).into())
                {
                    let target = DependencyNode::Named(&environment.name);
                    self.add_strong_dep(source, target);
                }
            }
        }
        Ok(())
    }

    fn service_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.service(),
            from: input.from_(),
            container: &self.all_services,
            all_dictionaries: &self.all_dictionaries,
            typename: "service",
        }
    }

    fn protocol_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.protocol(),
            from: input.from_(),
            container: &self.all_protocols,
            all_dictionaries: &self.all_dictionaries,
            typename: "protocol",
        }
    }

    fn directory_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.directory(),
            from: input.from_(),
            container: &self.all_directories,
            all_dictionaries: &self.all_dictionaries,
            typename: "directory",
        }
    }

    fn storage_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.storage(),
            from: input.from_(),
            container: &self.all_storages,
            all_dictionaries: &self.all_dictionaries,
            typename: "storage",
        }
    }

    fn runner_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.runner(),
            from: input.from_(),
            container: &self.all_runners,
            all_dictionaries: &self.all_dictionaries,
            typename: "runner",
        }
    }

    fn resolver_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.resolver(),
            from: input.from_(),
            container: &self.all_resolvers,
            all_dictionaries: &self.all_dictionaries,
            typename: "resolver",
        }
    }

    fn dictionary_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.dictionary(),
            from: input.from_(),
            container: &self.all_dictionaries,
            all_dictionaries: &self.all_dictionaries,
            typename: "dictionary",
        }
    }

    fn config_from_self_checker<'b>(
        &'b self,
        input: &'b (impl CapabilityClause + FromClause),
    ) -> RouteFromSelfChecker<'b> {
        RouteFromSelfChecker {
            capability_name: input.config(),
            from: input.from_(),
            container: &self.all_configs,
            all_dictionaries: &self.all_dictionaries,
            typename: "config",
        }
    }
}

/// Helper type that assists with validating declarations of `{use, offer, expose} from self`.
struct RouteFromSelfChecker<'a> {
    /// The value of the capability property (protocol, service, etc.)
    capability_name: Option<OneOrMany<&'a Name>>,

    /// The value of `from`.
    from: OneOrMany<AnyRef<'a>>,

    /// A [Container] which is used to check for the existence of a capability definition.
    container: &'a dyn Container,

    /// Reference to [ValidationContext::all_dictionaries].
    all_dictionaries: &'a HashMap<&'a Name, &'a Capability>,

    /// The string name for the capability's type.
    typename: &'static str,
}

impl<'a> RouteFromSelfChecker<'a> {
    fn validate(self, operand: &'static str) -> Result<(), Error> {
        let Self { capability_name, from, container, all_dictionaries, typename } = self;
        let Some(capability) = capability_name else {
            return Ok(());
        };
        for capability in capability {
            for from in &from {
                match from {
                    AnyRef::Self_ if !container.contains(capability) => {
                        return Err(Error::validate(format!(
                            "{typename} \"{capability}\" is {operand} from self, so it \
                            must be declared as a \"{typename}\" in \"capabilities\"",
                        )));
                    }
                    AnyRef::Dictionary(DictionaryRef { root: RootDictionaryRef::Self_, path }) => {
                        let first_segment = path.iter_segments().next().unwrap();
                        if !all_dictionaries.contains_key(first_segment) {
                            return Err(Error::validate(format!(
                                "{typename} \"{capability}\" is {operand} from \"self/{path}\", so \
                                \"{first_segment}\" must be declared as a \"dictionary\" in \"capabilities\"",
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// [Container] provides a capability type agnostic trait to check for the existence of a
/// capability definition of a particular type. This is useful for writing common validation
/// functions.
trait Container {
    fn contains(&self, key: &Name) -> bool;
}

impl<'a> Container for HashSet<&'a Name> {
    fn contains(&self, key: &Name) -> bool {
        self.contains(key)
    }
}

impl<'a, T> Container for HashMap<&'a Name, T> {
    fn contains(&self, key: &Name) -> bool {
        self.contains_key(key)
    }
}

// Construct the config type information out of a `use` for a configuration capability.
// This will return validation errors if the `use` is missing fields.
pub fn use_config_to_value_type(u: &Use) -> Result<ConfigValueType, Error> {
    let config = u.config.clone().expect("Only call use_config_to_value_type on a Config");

    let Some(config_type) = u.config_type.as_ref() else {
        return Err(Error::validate(format!("Config '{}' is missing field 'type'", config)));
    };

    let config_type = match config_type {
        ConfigType::Bool => ConfigValueType::Bool { mutability: None },
        ConfigType::Uint8 => ConfigValueType::Uint8 { mutability: None },
        ConfigType::Uint16 => ConfigValueType::Uint16 { mutability: None },
        ConfigType::Uint32 => ConfigValueType::Uint32 { mutability: None },
        ConfigType::Uint64 => ConfigValueType::Uint64 { mutability: None },
        ConfigType::Int8 => ConfigValueType::Int8 { mutability: None },
        ConfigType::Int16 => ConfigValueType::Int16 { mutability: None },
        ConfigType::Int32 => ConfigValueType::Int32 { mutability: None },
        ConfigType::Int64 => ConfigValueType::Int64 { mutability: None },
        ConfigType::String => {
            let Some(max_size) = u.config_max_size else {
                return Err(Error::validate(format!(
                    "Config '{}' is type String but is missing field 'max_size'",
                    config
                )));
            };
            ConfigValueType::String { max_size: max_size.into(), mutability: None }
        }
        ConfigType::Vector => {
            let Some(ref element) = u.config_element_type else {
                return Err(Error::validate(format!(
                    "Config '{}' is type Vector but is missing field 'element'",
                    config
                )));
            };
            let Some(max_count) = u.config_max_count else {
                return Err(Error::validate(format!(
                    "Config '{}' is type Vector but is missing field 'max_count'",
                    config
                )));
            };
            ConfigValueType::Vector {
                max_count: max_count.into(),
                element: element.clone(),
                mutability: None,
            }
        }
    };
    Ok(config_type)
}

/// Given an iterator with `(key, name)` tuples, ensure that `key` doesn't
/// appear twice. `name` is used in generated error messages.
fn ensure_no_duplicate_names<'a, I>(values: I) -> Result<(), Error>
where
    I: Iterator<Item = (&'a Name, &'a str)>,
{
    let mut seen_keys = HashMap::new();
    for (key, name) in values {
        if let Some(preexisting_name) = seen_keys.insert(key, name) {
            return Err(Error::validate(format!(
                "identifier \"{}\" is defined twice, once in \"{}\" and once in \"{}\"",
                key, name, preexisting_name
            )));
        }
    }
    Ok(())
}

/// Returns an error if the iterator contains duplicate values.
fn ensure_no_duplicate_values<'a, I, V>(values: I) -> Result<(), Error>
where
    I: IntoIterator<Item = &'a V>,
    V: 'a + Hash + Eq + fmt::Display,
{
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(Error::validate(format!("Found duplicate value \"{}\" in array.", value)));
        }
    }
    Ok(())
}

/// A node in the DependencyGraph. This enum is used to differentiate between node types.
#[derive(Copy, Clone, Hash, Ord, Debug, PartialOrd, PartialEq, Eq)]
enum DependencyNode<'a> {
    Named(&'a Name),
    Self_,
}

impl<'a> DependencyNode<'a> {
    fn from_dictionary_ref(d: &'a DictionaryRef) -> Option<DependencyNode<'a>> {
        match &d.root {
            RootDictionaryRef::Named(name) => Some(DependencyNode::Named(name)),
            RootDictionaryRef::Self_ => Some(DependencyNode::Self_),
            RootDictionaryRef::Parent => None,
        }
    }

    fn offer_to_ref(ref_: &'a OfferToRef) -> DependencyNode<'a> {
        match ref_ {
            OfferToRef::Named(name) => DependencyNode::Named(name),
            OfferToRef::All => panic!(r#"offer to "all" may not be in Dependency Graph"#),
            OfferToRef::OwnDictionary(name) => DependencyNode::Named(name),
        }
    }

    fn registration_ref(ref_: &'a RegistrationRef) -> Option<DependencyNode<'a>> {
        match ref_ {
            RegistrationRef::Named(name) => Some(DependencyNode::Named(name)),
            RegistrationRef::Self_ => Some(DependencyNode::Self_),

            // We don't care about cycles with the parent, because those will be resolved when the
            // parent manifest is validated.
            RegistrationRef::Parent => None,
        }
    }
}

impl<'a> fmt::Display for DependencyNode<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyNode::Self_ => write!(f, "self"),
            DependencyNode::Named(name) => write!(f, "#{}", name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Location;
    use crate::{
        offer_to_all_and_component_diff_protocols_message,
        offer_to_all_and_component_diff_sources_message,
    };
    use assert_matches::assert_matches;
    use serde_json::json;

    macro_rules! test_validate_cml {
        (
            $(
                $test_name:ident($input:expr, $($pattern:tt)+),
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    let input = format!("{}", $input);
                    let result = validate_for_test("test.cml", &input.as_bytes());
                    assert_matches!(result, $($pattern)+);
                }
            )+
        }
    }

    macro_rules! test_validate_cml_with_feature {
        (
            $features:expr,
            {
                $(
                    $test_name:ident($input:expr, $($pattern:tt)+),
                )+
            }
        ) => {
            $(
                #[test]
                fn $test_name() {
                    let input = format!("{}", $input);
                    let features = $features;
                    let result = validate_with_features_for_test("test.cml", &input.as_bytes(), &features, &vec![], &vec![]);
                    assert_matches!(result, $($pattern)+);
                }
            )+
        }
    }

    fn validate_for_test(filename: &str, input: &[u8]) -> Result<(), Error> {
        validate_with_features_for_test(filename, input, &FeatureSet::empty(), &[], &[])
    }

    fn validate_with_features_for_test(
        filename: &str,
        input: &[u8],
        features: &FeatureSet,
        required_offers: &[String],
        required_uses: &[String],
    ) -> Result<(), Error> {
        let input = format!("{}", std::str::from_utf8(input).unwrap().to_string());
        let file = Path::new(filename);
        let document = crate::parse_one_document(&input, &file)?;
        validate_cml(
            &document,
            Some(&file),
            &features,
            &ProtocolRequirements { must_offer: required_offers, must_use: required_uses },
        )
    }

    #[test]
    fn disable_required_offer() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
                {
                    name: "something_v2",
                    url: "fuchsia-pkg://fuchsia.com/something_v2#meta/something_v2.cm",
                },
            ],
            disable: {
                must_offer_protocol: [ "fuchsia.logger.LogSink" ],
            }
        }"##;
        let features = FeatureSet::empty();
        let result = validate_with_features_for_test(
            "test.cml",
            &input.as_bytes(),
            &features,
            &vec!["fuchsia.logger.LogSink".into()],
            &vec![],
        );
        assert!(result.is_ok());
    }

    fn unused_component_err_message(missing: &str) -> String {
        format!(r#"Protocol "{}" is not used by a component but is required by all"#, missing)
    }

    #[test]
    fn must_use_protocol() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &[],
            &vec!["fuchsia.logger.LogSink".into()],
        );

        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(err, unused_component_err_message("fuchsia.logger.LogSink"));
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );

        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
            ],

            use: [
                {
                    protocol: [ "fuchsia.component.Binder" ],
                    from: "framework",
                }
            ],
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &[],
            &vec!["fuchsia.component.Binder".into()],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn disable_must_use_protocol() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
                {
                    name: "something_v2",
                    url: "fuchsia-pkg://fuchsia.com/something_v2#meta/something_v2.cm",
                },
            ],

            disable: {
                must_use_protocol: [ "fuchsia.logger.LogSink" ],
            }
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &[],
            &vec!["fuchsia.logger.LogSink".into()],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn required_offer_to_all() {
        let input = r##"{
           children: [
               {
                   name: "logger",
                   url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
               },
               {
                   name: "something",
                   url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
               },
           ],
           collections: [
               {
                   name: "coll",
                   durability: "transient",
               },
           ],
           offer: [
               {
                   protocol: "fuchsia.logger.LogSink",
                   from: "parent",
                   to: "all"
               },
               {
                   protocol: "fuchsia.inspect.InspectSink",
                   from: "parent",
                   to: "all"
               },
               {
                   protocol: "fuchsia.process.Launcher",
                   from: "parent",
                   to: "#something",
               },
           ]
       }"##;
        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into(), "fuchsia.inspect.InspectSink".into()],
            &Vec::new(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn required_offer_to_all_manually() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            collections: [
                {
                    name: "coll",
                    durability: "transient",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "#something",
                    to: "#logger"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#something"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#coll",
                },
            ]
        }"##;
        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &[],
        );
        assert!(result.is_ok());

        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
                {
                    name: "something_v2",
                    url: "fuchsia-pkg://fuchsia.com/something_v2#meta/something_v2.cm",
                },
            ],
            collections: [
                {
                    name: "coll",
                    durability: "transient",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: ["#logger", "#something", "#something_v2", "#coll"],
                },
            ]
        }"##;
        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &[],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn offer_to_all_mixed_with_array_syntax() {
        let input = r##"{
                "children": [
                    {
                        "name": "something",
                        "url": "fuchsia-pkg://fuchsia.com/something/stable#meta/something.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": ["fuchsia.logger.LogSink", "fuchsia.inspect.InspectSink",],
                        "from": "parent",
                        "to": "#something",
                    },
                    {
                        "protocol": "fuchsia.logger.LogSink",
                        "from": "parent",
                        "to": "all",
                    },
                ],
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        assert!(result.is_ok(), "{:#?}", result);

        let input = r##"{

                "children": [
                    {
                        "name": "something",
                        "url": "fuchsia-pkg://fuchsia.com/something/stable#meta/something.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": ["fuchsia.logger.LogSink", "fuchsia.inspect.InspectSink",],
                        "from": "parent",
                        "to": "all",
                    },
                    {
                        "protocol": "fuchsia.logger.LogSink",
                        "from": "parent",
                        "to": "#something",
                    },
                ],
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        assert!(result.is_ok(), "{:#?}", result);
    }

    #[test]
    fn offer_to_all_and_manual() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "all"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#something"
                },
            ]
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        // exact duplication is allowed
        assert!(result.is_ok(), "{:#?}", result);

        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "all"
                },
                {
                    protocol: "fuchsia.logger.FakLog",
                    from: "parent",
                    as: "fuchsia.logger.LogSink",
                    to: "#something"
                },
            ]
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        // aliased duplications are forbidden
        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(
                    err,
                    offer_to_all_and_component_diff_protocols_message(&["fuchsia.logger.LogSink"], "something"),
                );
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );

        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "all"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "framework",
                    to: "#something"
                },
            ]
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        // offering the same protocol without an alias from different sources is forbidden
        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(
                    err,
                    offer_to_all_and_component_diff_sources_message(&["fuchsia.logger.LogSink"], "something"),
                );
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );
    }

    fn offer_to_all_diff_sources_message(protocols: &[&str]) -> String {
        format!(r#"Protocol(s) {:?} offered to "all" multiple times"#, protocols)
    }

    #[test]
    fn offer_to_all_from_diff_sources() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "all"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "framework",
                    to: "all"
                },
            ]
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(
                    err,
                    offer_to_all_diff_sources_message(&["fuchsia.logger.LogSink"]),
                );
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );
    }

    #[test]
    fn offer_to_all_with_aliases() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "all"
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "framework",
                    to: "all",
                    as: "OtherLogSink",
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "framework",
                    to: "#something",
                    as: "OtherOtherLogSink",
                },
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#something",
                    as: "fuchsia.logger.LogSink",
                },
            ]
        }"##;

        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &Vec::new(),
        );

        assert!(result.is_ok());
    }

    fn fail_to_make_required_offer(
        protocol: &str,
        child_or_collection: &str,
        component: &str,
    ) -> String {
        format!(
            r#"Protocol "{}" is not offered to {} "{}" but it is a required offer"#,
            protocol, child_or_collection, component
        )
    }

    #[test]
    fn fail_to_offer_to_all_when_required() {
        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
                {
                    name: "something",
                    url: "fuchsia-pkg://fuchsia.com/something#meta/something.cm",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#logger"
                },
                {
                    protocol: "fuchsia.logger.LegacyLog",
                    from: "parent",
                    to: "#something"
                },
            ]
        }"##;
        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &[],
        );

        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(
                    err,
                    fail_to_make_required_offer(
                        "fuchsia.logger.LogSink",
                        "child component",
                        "something",
                    ),
                );
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );

        let input = r##"{
            children: [
                {
                    name: "logger",
                    url: "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                },
            ],
            collections: [
                {
                    name: "coll",
                    durability: "transient",
                },
            ],
            offer: [
                {
                    protocol: "fuchsia.logger.LogSink",
                    from: "parent",
                    to: "#logger"
                },
            ]
        }"##;
        let result = validate_with_features_for_test(
            "test.cml",
            input.as_bytes(),
            &FeatureSet::empty(),
            &vec!["fuchsia.logger.LogSink".into()],
            &[],
        );

        assert_matches!(result,
            Err(Error::Validate { err, filename }) => {
                assert_eq!(
                    err,
                    fail_to_make_required_offer("fuchsia.logger.LogSink", "collection", "coll"),
                );
                assert!(filename.is_some(), "Expected there to be a filename in error message");
            }
        );
    }

    #[test]
    fn test_validate_invalid_json_fails() {
        let result = validate_for_test("test.cml", b"{");
        let expected_err = r#" --> 1:2
  |
1 | {
  |  ^---
  |
  = expected identifier or string"#;
        assert_matches!(result, Err(Error::Parse { err, .. }) if &err == expected_err);
    }

    #[test]
    fn test_cml_json5() {
        let input = r##"{
            "expose": [
                // Here are some services to expose.
                { "protocol": "fuchsia.logger.Log", "from": "#logger", },
                { "directory": "blobfs", "from": "#logger", "rights": ["rw*"]},
            ],
            "children": [
                {
                    name: 'logger',
                    'url': 'fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm',
                },
            ],
        }"##;
        let result = validate_for_test("test.cml", input.as_bytes());
        assert_matches!(result, Ok(()));
    }

    #[test]
    fn test_cml_error_location() {
        let input = r##"{
    "use": [
        {
            "protocol": "foo",
            "from": "bad",
        },
    ],
}"##;
        let result = validate_for_test("test.cml", input.as_bytes());
        assert_matches!(
            result,
            Err(Error::Parse { err, location: Some(l), filename: Some(f) })
                if &err == "invalid value: string \"bad\", expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none" &&
                l == Location { line: 5, column: 21 } &&
                f.ends_with("test.cml")
        );
    }

    test_validate_cml! {
        // include
        test_cml_empty_include(
            json!(
                {
                    "include": [],
                }
            ),
            Ok(())
        ),
        test_cml_some_include(
            json!(
                {
                    "include": [ "some.cml" ],
                }
            ),
            Ok(())
        ),
        test_cml_couple_of_include(
            json!(
                {
                    "include": [ "some1.cml", "some2.cml" ],
                }
            ),
            Ok(())
        ),

        // program
        test_cml_empty_json(
            json!({}),
            Ok(())
        ),
        test_cml_program(
            json!(
                {
                    "program": {
                        "runner": "elf",
                        "binary": "bin/app",
                    },
                }
            ),
            Ok(())
        ),
        test_cml_program_use_runner(
            json!(
                {
                    "program": {
                        "binary": "bin/app",
                    },
                    "use": [
                        { "runner": "elf", "from": "parent" }
                    ]
                }
            ),
            Ok(())
        ),
        test_cml_program_use_runner_conflict(
            json!(
                {
                    "program": {
                        "runner": "elf",
                        "binary": "bin/app",
                    },
                    "use": [
                        { "runner": "elf", "from": "parent" }
                    ]
                }
            ),
            Err(Error::Validate { err, .. }) if &err ==
                "Component has conflicting runners in `program` block and `use` block."
        ),
        test_cml_program_no_runner(
            json!({"program": { "binary": "bin/app" }}),
            Err(Error::Validate { err, .. }) if &err ==
                "Component has a `program` block defined, but doesn't specify a `runner`. \
                Components need to use a runner to actually execute code."
        ),

        // use
        test_cml_use(
            json!({
                "use": [
                  { "protocol": "CoolFonts", "path": "/svc/MyFonts" },
                  { "protocol": "CoolFonts2", "path": "/svc/MyFonts2", "from": "parent/dict" },
                  { "protocol": "fuchsia.test.hub.HubReport", "from": "framework" },
                  { "protocol": "fuchsia.sys2.StorageAdmin", "from": "#data-storage" },
                  { "protocol": ["fuchsia.ui.scenic.Scenic", "fuchsia.logger.LogSink"] },
                  {
                    "directory": "assets",
                    "path": "/data/assets",
                    "rights": ["rw*"],
                  },
                  {
                    "directory": "config",
                    "from": "parent",
                    "path": "/data/config",
                    "rights": ["rx*"],
                    "subdir": "fonts/all",
                  },
                  { "storage": "data", "path": "/example" },
                  { "storage": "cache", "path": "/tmp" },
                  {
                   "event_stream": ["started", "stopped", "running"],
                   "scope":["#test"],
                   "path":"/svc/testpath",
                   "from":"parent",
                  },
                  { "runner": "usain", "from": "parent" }
                ],
                "capabilities": [
                    {
                        "storage": "data-storage",
                        "from": "parent",
                        "backing_dir": "minfs",
                        "storage_id": "static_instance_id_or_moniker",
                    }
                ]
            }),
            Ok(())
        ),
        test_cml_expose_event_stream_multiple_as(
            json!({
                "expose": [
                    {
                        "event_stream": ["started", "stopped"],
                        "from" : "framework",
                        "as": "something"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "as cannot be used with multiple event streams"
        ),
        test_cml_offer_event_stream_capability_requested_not_from_framework(
            json!({
                "offer": [
                    {
                        "event_stream": ["capability_requested", "stopped"],
                        "from" : "parent",
                        "to": "#something"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"#something\" is an \"offer\" target from \"parent\" but it does not appear in \"children\" or \"collections\""
        ),
        test_cml_offer_event_stream_capability_requested_with_filter(
            json!({
                "offer": [
                    {
                        "event_stream": "capability_requested",
                        "from" : "framework",
                        "to": "#something",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"#something\" is an \"offer\" target from \"framework\" but it does not appear in \"children\" or \"collections\""
        ),
        test_cml_offer_event_stream_multiple_as(
            json!({
                "offer": [
                    {
                        "event_stream": ["started", "stopped"],
                        "from" : "framework",
                        "to": "#self",
                        "as": "something"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "as cannot be used with multiple events"
        ),
        test_cml_expose_event_stream_from_self(
            json!({
                "expose": [
                    { "event_stream": ["started", "stopped"], "from" : "self" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "Cannot expose event_streams from self"
        ),
        test_cml_offer_event_stream_from_self(
            json!({
                "offer": [
                    { "event_stream": ["started", "stopped"], "from" : "self", "to": "#self" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "cannot offer an event_stream from self"
        ),
        test_cml_offer_event_stream_from_anything_else(
            json!({
                "offer": [
                    {
                        "event_stream": ["started", "stopped"],
                        "from" : "framework",
                        "to": "#self"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"#self\" is an \"offer\" target from \"framework\" but it does not appear in \"children\" or \"collections\""
        ),
        test_cml_expose_event_stream_to_framework(
            json!({
                "expose": [
                    {
                        "event_stream": ["started", "stopped"],
                        "from" : "self",
                        "to": "framework"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "cannot expose an event_stream to framework"
        ),
        test_cml_expose_event_stream_scope_invalid_component(
            json!({
                "expose": [
                    {
                        "event_stream": ["started", "stopped"],
                        "from" : "framework",
                        "scope":["#invalid_component"]
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "event_stream scope invalid_component did not match a component or collection in this .cml file."
        ),

        test_cml_use_from_self(
            json!({
                "use": [
                    {
                        "protocol": [ "bar_protocol", "baz_protocol" ],
                        "from": "self",
                    },
                    {
                        "directory": "foo_directory",
                        "from": "self",
                        "path": "/dir",
                        "rights": [ "r*" ],
                    },
                    {
                        "service": "foo_service",
                        "from": "self",
                    },
                    {
                        "config": "foo_config",
                        "type": "bool",
                        "key": "k",
                        "from": "self",
                    },
                ],
                "capabilities": [
                    {
                        "protocol": "bar_protocol",
                    },
                    {
                        "protocol": "baz_protocol",
                    },
                    {
                        "directory": "foo_directory",
                        "path": "/dir",
                        "rights": [ "r*" ],
                    },
                    {
                        "service": "foo_service",
                    },
                    {
                        "config": "foo_config",
                        "type": "bool",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_use_protocol_from_self_missing(
            json!({
                "use": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is used from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_use_directory_from_self_missing(
            json!({
                "use": [
                    {
                        "directory": "foo_directory",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"foo_directory\" is used from self, so it must be declared as a \"directory\" in \"capabilities\""
        ),
        test_cml_use_service_from_self_missing(
            json!({
                "use": [
                    {
                        "service": "foo_service",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "service \"foo_service\" is used from self, so it must be declared as a \"service\" in \"capabilities\""
        ),
        test_cml_use_config_from_self_missing(
            json!({
                "use": [
                    {
                        "config": "foo_config",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "config \"foo_config\" is used from self, so it must be declared as a \"config\" in \"capabilities\""
        ),
        test_cml_use_from_self_missing_dictionary(
            json!({
                "use": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self/dict/inner",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is used from \"self/dict/inner\", so \"dict\" must be declared as a \"dictionary\" in \"capabilities\""
        ),
        test_cml_use_event_stream_duplicate(
            json!({
                "use": [
                    { "event_stream": ["started", "started"], "from" : "parent" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: array with duplicate element, expected a name or nonempty array of names, with unique elements"
        ),
        test_cml_use_event_stream_overlapping_path(
            json!({
                "use": [
                    { "directory": "foobarbaz", "path": "/foo/bar/baz", "rights": [ "r*" ] },
                    {
                        "event_stream": ["started"],
                        "path": "/foo/bar/baz/er",
                        "from": "parent",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/foo/bar/baz\" is a prefix of \"use\" target event_stream \"/foo/bar/baz/er\""
        ),
        test_cml_use_event_stream_invalid_path(
            json!({
                "use": [
                    {
                        "event_stream": ["started"],
                        "path": "my_stream",
                        "from": "parent",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"my_stream\", expected a path with leading `/` and non-empty segments, where each segment is no more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., and cannot contain embedded NULs"
        ),
        test_cml_use_event_stream_self_ref(
            json!({
                "use": [
                    {
                        "event_stream": ["started"],
                        "path": "/svc/my_stream",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"from: self\" cannot be used with \"event_stream\""
        ),
        test_cml_use_runner_debug_ref(
            json!({
                "use": [
                    {
                        "runner": "elf",
                        "from": "debug",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "only \"protocol\" supports source from \"debug\""
        ),
        test_cml_use_runner_self_ref(
            json!({
                "use": [
                    {
                        "runner": "elf",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"from: self\" cannot be used with \"runner\""
        ),
        test_cml_use_missing_props(
            json!({
                "use": [ { "path": "/svc/fuchsia.logger.Log" } ]
            }),
            Err(Error::Validate { err, .. }) if &err == "`use` declaration is missing a capability keyword, one of: \"service\", \"protocol\", \"directory\", \"storage\", \"event_stream\", \"runner\", \"config\""
        ),
        test_cml_use_from_with_storage(
            json!({
                "use": [ { "storage": "cache", "from": "parent" } ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"from\" cannot be used with \"storage\""
        ),
        test_cml_use_invalid_from(
            json!({
                "use": [
                  { "protocol": "CoolFonts", "from": "bad" }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bad\", expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
        ),
        test_cml_use_invalid_from_dictionary(
            json!({
                "use": [
                  { "protocol": "CoolFonts", "from": "bad/dict" }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bad/dict\", expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
        ),
        test_cml_use_from_missing_capability(
            json!({
                "use": [
                  { "protocol": "fuchsia.sys2.Admin", "from": "#mystorage" }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"use\" source \"#mystorage\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_use_bad_path(
            json!({
                "use": [
                    {
                        "protocol": ["CoolFonts", "FunkyFonts"],
                        "path": "/MyFonts"
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" can only be specified when one `protocol` is supplied."
        ),
        test_cml_use_bad_duplicate_target_names(
            json!({
                "use": [
                  { "protocol": "fuchsia.component.Realm" },
                  { "protocol": "fuchsia.component.Realm" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"/svc/fuchsia.component.Realm\" is a duplicate \"use\" target protocol"
        ),
        test_cml_use_empty_protocols(
            json!({
                "use": [
                    {
                        "protocol": [],
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a name or nonempty array of names, with unique elements"
        ),
        test_cml_use_bad_subdir(
            json!({
                "use": [
                  {
                    "directory": "config",
                    "path": "/config",
                    "from": "parent",
                    "rights": [ "r*" ],
                    "subdir": "/",
                  },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_use_resolver_fails(
            json!({
                "use": [
                    {
                        "resolver": "pkg_resolver",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if err.starts_with("unknown field `resolver`, expected one of")
        ),

        test_cml_use_disallows_nested_dirs_directory(
            json!({
                "use": [
                    { "directory": "foobar", "path": "/foo/bar", "rights": [ "r*" ] },
                    { "directory": "foobarbaz", "path": "/foo/bar/baz", "rights": [ "r*" ] },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/foo/bar\" is a prefix of \"use\" target directory \"/foo/bar/baz\""
        ),
        test_cml_use_disallows_nested_dirs_storage(
            json!({
                "use": [
                    { "storage": "foobar", "path": "/foo/bar" },
                    { "storage": "foobarbaz", "path": "/foo/bar/baz" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "storage \"/foo/bar\" is a prefix of \"use\" target storage \"/foo/bar/baz\""
        ),
        test_cml_use_disallows_nested_dirs_directory_and_storage(
            json!({
                "use": [
                    { "directory": "foobar", "path": "/foo/bar", "rights": [ "r*" ] },
                    { "storage": "foobarbaz", "path": "/foo/bar/baz" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/foo/bar\" is a prefix of \"use\" target storage \"/foo/bar/baz\""
        ),
        test_cml_use_disallows_common_prefixes_service(
            json!({
                "use": [
                    { "directory": "foobar", "path": "/foo/bar", "rights": [ "r*" ] },
                    { "protocol": "fuchsia", "path": "/foo/bar/fuchsia" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/foo/bar\" is a prefix of \"use\" target protocol \"/foo/bar/fuchsia\""
        ),
        test_cml_use_disallows_common_prefixes_protocol(
            json!({
                "use": [
                    { "directory": "foobar", "path": "/foo/bar", "rights": [ "r*" ] },
                    { "protocol": "fuchsia", "path": "/foo/bar/fuchsia.2" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/foo/bar\" is a prefix of \"use\" target protocol \"/foo/bar/fuchsia.2\""
        ),
        test_cml_use_disallows_pkg_conflicts_for_directories(
            json!({
                "use": [
                    { "directory": "dir", "path": "/pkg/dir", "rights": [ "r*" ] },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"/pkg/dir\" conflicts with the protected path \"/pkg\", please use this capability with a different path"
        ),
        test_cml_use_disallows_pkg_conflicts_for_protocols(
            json!({
                "use": [
                    { "protocol": "prot", "path": "/pkg/protocol" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"/pkg/protocol\" conflicts with the protected path \"/pkg\", please use this capability with a different path"
        ),
        test_cml_use_disallows_pkg_conflicts_for_storage(
            json!({
                "use": [
                    { "storage": "store", "path": "/pkg/storage" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "storage \"/pkg/storage\" conflicts with the protected path \"/pkg\", please use this capability with a different path"
        ),
        test_cml_use_disallows_filter_on_non_events(
            json!({
                "use": [
                    { "directory": "foobar", "path": "/foo/bar", "rights": [ "r*" ], "filter": {"path": "/diagnostics"} },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"filter\" can only be used with \"event_stream\""
        ),
        test_cml_availability_not_supported_for_event_streams(
            json!({
                "use": [
                    {
                        "event_stream": ["destroyed"],
                        "from": "parent",
                        "availability": "optional",
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"availability\" cannot be used with \"event_stream\""
        ),
        test_cml_use_from_child_offer_cycle_strong(
            json!({
                "capabilities": [
                    { "protocol": ["fuchsia.example.Protocol"] },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/child#meta/child.cm",
                    },
                ],
                "use": [
                    {
                        "protocol": "fuchsia.child.Protocol",
                        "from": "#child",
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "self",
                        "to": [ "#child" ],
                    },
                ],
            }),
            Err(Error::Validate { err, .. })
                if &err == "Strong dependency cycles were found. Break the cycle by removing a \
                            dependency or marking an offer as weak. Cycles: \
                            {{#child -> self -> #child}}"
        ),
        test_cml_use_from_parent_weak(
            json!({
                "use": [
                    {
                        "protocol": "fuchsia.parent.Protocol",
                        "from": "parent",
                        "dependency": "weak",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "Only `use` from children can have dependency: \"weak\""
        ),
        test_cml_use_from_child_offer_cycle_weak(
            json!({
                "capabilities": [
                    { "protocol": ["fuchsia.example.Protocol"] },
                    { "service": ["fuchsia.example.Service"] },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/child#meta/child.cm",
                    },
                ],
                "use": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                        "dependency": "weak",
                    },
                    {
                        "service": "fuchsia.example.Service",
                        "from": "#child",
                        "dependency": "weak",
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "self",
                        "to": [ "#child" ],
                    },
                    {
                        "service": "fuchsia.example.Service",
                        "from": "self",
                        "to": [ "#child" ],
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_use_from_child_offer_storage_no_cycle(
            json!({
                "capabilities": [
                    {
                        "storage": "cdata",
                        "from": "#backend",
                        "backing_dir": "blobfs",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                    {
                        "storage": "pdata",
                        "from": "parent",
                        "backing_dir": "blobfs",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "#meta/child.cm",
                    },
                    {
                        "name": "backend",
                        "url": "#meta/backend.cm",
                    },
                ],
                "use": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                    },
                ],
                "offer": [
                    {
                        "storage": "cdata",
                        "from": "self",
                        "to": "#child",
                    },
                    {
                        "storage": "pdata",
                        "from": "self",
                        "to": "#child",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_use_from_child_offer_storage_cycle(
            json!({
                "capabilities": [
                    {
                        "storage": "data",
                        "from": "self",
                        "backing_dir": "blobfs",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "#meta/child.cm",
                    },
                ],
                "use": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                    },
                ],
                "offer": [
                    {
                        "storage": "data",
                        "from": "self",
                        "to": "#child",
                    },
                ],
            }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a dependency \
                or marking an offer as weak. Cycles: {{#child -> self -> #data -> #child}}"
        ),

        // expose
        test_cml_expose(
            json!({
                "expose": [
                    {
                        "protocol": "A",
                        "from": "self",
                    },
                    {
                        "protocol": ["B", "C"],
                        "from": "self",
                    },
                    {
                        "protocol": "D",
                        "from": "#mystorage",
                    },
                    {
                        "directory": "blobfs",
                        "from": "self",
                        "rights": ["r*"],
                        "subdir": "blob",
                    },
                    { "directory": "data", "from": "framework" },
                    { "runner": "elf", "from": "#logger",  },
                    { "resolver": "pkg_resolver", "from": "#logger" },
                ],
                "capabilities": [
                    { "protocol": ["A", "B", "C"] },
                    {
                        "directory": "blobfs",
                        "path": "/blobfs",
                        "rights": ["rw*"],
                    },
                    {
                        "storage": "mystorage",
                        "from": "self",
                        "backing_dir": "blobfs",
                        "storage_id": "static_instance_id_or_moniker",
                    }
                ],
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_expose_all_valid_chars(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.logger.Log",
                        "from": "#abcdefghijklmnopqrstuvwxyz0123456789_-.",
                    },
                ],
                "children": [
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-.",
                        "url": "https://www.google.com/gmail"
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_expose_missing_props(
            json!({
                "expose": [ {} ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `from`"
        ),
        test_cml_expose_missing_from(
            json!({
                "expose": [
                    { "protocol": "fuchsia.logger.Log", "from": "#missing" },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#missing\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_expose_duplicate_target_names(
            json!({
                "capabilities": [
                    { "protocol": "logger" },
                ],
                "expose": [
                    { "protocol": "logger", "from": "self", "as": "thing" },
                    { "directory": "thing", "from": "#child" , "rights": ["rx*"] },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/pkg#comp.cm",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"thing\" is a duplicate \"expose\" target capability for \"parent\""
        ),
        test_cml_expose_invalid_multiple_from(
            json!({
                    "capabilities": [
                        { "protocol": "fuchsia.logger.Log" },
                    ],
                    "expose": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": [ "self", "#logger" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "logger",
                            "url": "fuchsia-pkg://fuchsia.com/logger#meta/logger.cm",
                        },
                    ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"protocol\" capabilities cannot have multiple \"from\" clauses"
        ),
        test_cml_expose_from_missing_named_source(
            json!({
                    "expose": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": "#does-not-exist",
                        },
                    ],
                }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#does-not-exist\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_expose_bad_from(
            json!({
                "expose": [ {
                    "protocol": "fuchsia.logger.Log", "from": "parent"
                } ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"parent\", expected one or an array of \"framework\", \"self\", \"#<child-name>\", or a dictionary path"
        ),
        // if "as" is specified, only 1 array item is allowed.
        test_cml_expose_bad_as(
            json!({
                "expose": [
                    {
                        "protocol": ["A", "B"],
                        "from": "#echo_server",
                        "as": "thing"
                    },
                ],
                "children": [
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"as\" can only be specified when one `protocol` is supplied."
        ),
        test_cml_expose_empty_protocols(
            json!({
                "expose": [
                    {
                        "protocol": [],
                        "from": "#child",
                        "as": "thing"
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/pkg#comp.cm",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a name or nonempty array of names, with unique elements"
        ),
        test_cml_expose_bad_subdir(
            json!({
                "expose": [
                    {
                        "directory": "blobfs",
                        "from": "self",
                        "rights": ["r*"],
                        "subdir": "/",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_expose_invalid_subdir_to_framework(
            json!({
                "capabilities": [
                    {
                        "directory": "foo",
                        "rights": ["r*"],
                        "path": "/foo",
                    },
                ],
                "expose": [
                    {
                        "directory": "foo",
                        "from": "self",
                        "to": "framework",
                        "subdir": "blob",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/pkg#comp.cm",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "`subdir` is not supported for expose to framework. Directly expose the subdirectory instead."
        ),
        test_cml_expose_from_self(
            json!({
                "expose": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self",
                    },
                    {
                        "protocol": [ "bar_protocol", "baz_protocol" ],
                        "from": "self",
                    },
                    {
                        "directory": "foo_directory",
                        "from": "self",
                    },
                    {
                        "runner": "foo_runner",
                        "from": "self",
                    },
                    {
                        "resolver": "foo_resolver",
                        "from": "self",
                    },
                ],
                "capabilities": [
                    {
                        "protocol": "foo_protocol",
                    },
                    {
                        "protocol": "bar_protocol",
                    },
                    {
                        "protocol": "baz_protocol",
                    },
                    {
                        "directory": "foo_directory",
                        "path": "/dir",
                        "rights": [ "r*" ],
                    },
                    {
                        "runner": "foo_runner",
                        "path": "/svc/runner",
                    },
                    {
                        "resolver": "foo_resolver",
                        "path": "/svc/resolver",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_expose_protocol_from_self_missing(
            json!({
                "expose": [
                    {
                        "protocol": "pkg_protocol",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"pkg_protocol\" is exposed from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_expose_protocol_from_self_missing_multiple(
            json!({
                "expose": [
                    {
                        "protocol": [ "foo_protocol", "bar_protocol" ],
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is exposed from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_expose_directory_from_self_missing(
            json!({
                "expose": [
                    {
                        "directory": "pkg_directory",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"pkg_directory\" is exposed from self, so it must be declared as a \"directory\" in \"capabilities\""
        ),
        test_cml_expose_service_from_self_missing(
            json!({
                "expose": [
                    {
                        "service": "pkg_service",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "service \"pkg_service\" is exposed from self, so it must be declared as a \"service\" in \"capabilities\""
        ),
        test_cml_expose_runner_from_self_missing(
            json!({
                "expose": [
                    {
                        "runner": "dart",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "runner \"dart\" is exposed from self, so it must be declared as a \"runner\" in \"capabilities\""
        ),
        test_cml_expose_resolver_from_self_missing(
            json!({
                "expose": [
                    {
                        "resolver": "pkg_resolver",
                        "from": "self",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "resolver \"pkg_resolver\" is exposed from self, so it must be declared as a \"resolver\" in \"capabilities\""
        ),
        test_cml_expose_from_self_missing_dictionary(
            json!({
                "expose": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self/dict/inner",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is exposed from \"self/dict/inner\", so \"dict\" must be declared as a \"dictionary\" in \"capabilities\""
        ),
        test_cml_expose_from_dictionary_invalid(
            json!({
                "expose": [
                    {
                        "protocol": "pkg_protocol",
                        "from": "bad/a",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bad/a\", expected one or an array of \"framework\", \"self\", \"#<child-name>\", or a dictionary path"
        ),
        test_cml_expose_from_dictionary_parent(
            json!({
                "expose": [
                    {
                        "protocol": "pkg_protocol",
                        "from": "parent/a",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "`expose` dictionary path must begin with `self` or `#<child-name>`"
        ),
                test_cml_expose_protocol_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "expose": [
                    { "protocol": "fuchsia.logger.Log", "from": "#coll" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#coll\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_expose_directory_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "expose": [
                    { "directory": "temp", "from": "#coll" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_expose_runner_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "expose": [
                    { "runner": "elf", "from": "#coll" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_expose_resolver_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "expose": [
                    { "resolver": "base", "from": "#coll" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_expose_to_framework_ok(
            json!({
                "capabilities": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                    },
                ],
                "expose": [
                    {
                        "directory": "foo",
                        "from": "self",
                        "to": "framework"
                    }
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/pkg#comp.cm",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_expose_to_framework_invalid(
            json!({
                "expose": [
                    {
                        "directory": "foo",
                        "from": "#logger",
                        "to": "framework"
                    }
                ],
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "Expose to framework can only be done from self."
        ),

        // offer
        test_cml_offer(
            json!({
                "offer": [
                    {
                        "protocol": "fuchsia.fonts.LegacyProvider",
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "dependency": "weak"
                    },
                    {
                        "protocol": "fuchsia.sys2.StorageAdmin",
                        "from": "#data",
                        "to": [ "#echo_server" ]
                    },
                    {
                        "protocol": [
                            "fuchsia.settings.Accessibility",
                            "fuchsia.ui.scenic.Scenic"
                        ],
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "dependency": "strong"
                    },
                    {
                        "directory": "assets",
                        "from": "self",
                        "to": [ "#echo_server" ],
                        "rights": ["r*"]
                    },
                    {
                        "directory": "index",
                        "subdir": "files",
                        "from": "parent",
                        "to": [ "#modular" ],
                        "dependency": "weak"
                    },
                    {
                        "directory": "config",
                        "from": "framework",
                        "to": [ "#modular" ],
                        "as": "config",
                        "dependency": "strong"
                    },
                    {
                        "storage": "data",
                        "from": "self",
                        "to": [ "#modular", "#logger" ]
                    },
                    {
                        "runner": "elf",
                        "from": "parent",
                        "to": [ "#modular", "#logger" ]
                    },
                    {
                        "resolver": "pkg_resolver",
                        "from": "parent",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    },
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                    },
                ],
                "collections": [
                    {
                        "name": "modular",
                        "durability": "transient",
                    },
                ],
                "capabilities": [
                    {
                        "directory": "assets",
                        "path": "/data/assets",
                        "rights": [ "rw*" ],
                    },
                    {
                        "storage": "data",
                        "from": "parent",
                        "backing_dir": "minfs",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_offer_all_valid_chars(
            json!({
                "offer": [
                    {
                        "protocol": "fuchsia.logger.Log",
                        "from": "#abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "to": [ "#abcdefghijklmnopqrstuvwxyz0123456789_-to" ],
                    },
                ],
                "children": [
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "url": "https://www.google.com/gmail"
                    },
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-to",
                        "url": "https://www.google.com/gmail"
                    },
                ],
                "capabilities": [
                    {
                        "storage": "abcdefghijklmnopqrstuvwxyz0123456789_-storage",
                        "from": "#abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "backing_dir": "example",
                        "storage_id": "static_instance_id_or_moniker",
                    }
                ]
            }),
            Ok(())
        ),
        test_cml_offer_singleton_to (
            json!({
                "offer": [
                    {
                        "protocol": "fuchsia.fonts.LegacyProvider",
                        "from": "parent",
                        "to": "#echo_server",
                        "dependency": "weak"
                    },
                ],
                "children": [
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_offer_missing_props(
            json!({
                "offer": [ {} ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `from`"
        ),
        test_cml_offer_missing_from(
            json!({
                    "offer": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": "#missing",
                            "to": [ "#echo_server" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "echo_server",
                            "url": "fuchsia-pkg://fuchsia.com/echo_server#meta/echo_server.cm",
                        },
                    ],
                }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#missing\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_storage_offer_from_child(
            json!({
                    "offer": [
                        {
                            "storage": "cache",
                            "from": "#storage_provider",
                            "to": [ "#echo_server" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "echo_server",
                            "url": "fuchsia-pkg://fuchsia.com/echo_server#meta/echo_server.cm",
                        },
                        {
                            "name": "storage_provider",
                            "url": "fuchsia-pkg://fuchsia.com/storage_provider#meta/storage_provider.cm",
                        },
                    ],
                }),
            Err(Error::Validate { err, .. }) if &err == "Storage \"cache\" is offered from a child, but storage capabilities cannot be exposed"
        ),
        test_cml_offer_bad_from(
            json!({
                    "offer": [ {
                        "protocol": "fuchsia.logger.Log",
                        "from": "#invalid@",
                        "to": [ "#echo_server" ],
                    } ]
                }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"#invalid@\", expected one or an array of \"parent\", \"framework\", \"self\", \"#<child-name>\", \"#<collection-name>\", or a dictionary path"
        ),
        test_cml_offer_invalid_multiple_from(
            json!({
                    "offer": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": [ "parent", "#logger" ],
                            "to": [ "#echo_server" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "logger",
                            "url": "fuchsia-pkg://fuchsia.com/logger#meta/logger.cm",
                        },
                        {
                            "name": "echo_server",
                            "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                        },
                    ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"protocol\" capabilities cannot have multiple \"from\" clauses"
        ),
        test_cml_offer_from_missing_named_source(
            json!({
                    "offer": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": "#does-not-exist",
                            "to": ["#echo_server" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "echo_server",
                            "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                        },
                    ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#does-not-exist\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_offer_protocol_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                } ],
                "offer": [
                    { "protocol": "fuchsia.logger.Log", "from": "#coll", "to": [ "#echo_server" ] },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#coll\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_offer_directory_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                } ],
                "offer": [
                    { "directory": "temp", "from": "#coll", "to": [ "#echo_server" ] },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_offer_storage_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                } ],
                "offer": [
                    { "storage": "cache", "from": "#coll", "to": [ "#echo_server" ] },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "Storage \"cache\" is offered from a child, but storage capabilities cannot be exposed"
        ),
        test_cml_offer_runner_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                } ],
                "offer": [
                    { "runner": "elf", "from": "#coll", "to": [ "#echo_server" ] },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_offer_resolver_from_collection_invalid(
            json!({
                "collections": [ {
                    "name": "coll",
                    "durability": "transient",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                } ],
                "offer": [
                    { "resolver": "base", "from": "#coll", "to": [ "#echo_server" ] },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#coll\" does not appear in \"children\""
        ),
        test_cml_offer_from_dictionary_invalid(
            json!({
                "offer": [
                    {
                        "protocol": "pkg_protocol",
                        "from": "bad/a",
                        "to": "#child",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://child",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bad/a\", expected one or an array of \"parent\", \"framework\", \"self\", \"#<child-name>\", \"#<collection-name>\", or a dictionary path"
        ),
        test_cml_offer_to_non_dictionary(
            json!({
                "offer": [
                    {
                        "protocol": "p",
                        "from": "parent",
                        "to": "self/dict",
                    },
                ],
                "capabilities": [
                    {
                        "protocol": "dict",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" has dictionary target \
            \"self/dict\" but \"dict\" is not a dictionary capability defined by \
            this component"
        ),

        test_cml_offer_empty_targets(
            json!({
                "offer": [
                    {
                        "protocol": "fuchsia.logger.Log",
                        "from": "#child",
                        "to": []
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/pkg#comp.cm",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected one or an array of \"#<child-name>\", \"#<collection-name>\", or \"self/<dictionary>\", with unique elements"
        ),
        test_cml_offer_duplicate_targets(
            json!({
                "offer": [ {
                    "protocol": "fuchsia.logger.Log",
                    "from": "#logger",
                    "to": ["#a", "#a"]
                } ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: array with duplicate element, expected one or an array of \"#<child-name>\", \"#<collection-name>\", or \"self/<dictionary>\", with unique elements"
        ),
        test_cml_offer_target_missing_props(
            json!({
                "offer": [ {
                    "protocol": "fuchsia.logger.Log",
                    "from": "#logger",
                    "as": "fuchsia.logger.SysLog",
                } ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `to`"
        ),
        test_cml_offer_target_missing_to(
            json!({
                "offer": [ {
                    "protocol": "fuchsia.logger.Log",
                    "from": "#logger",
                    "to": [ "#missing" ],
                } ],
                "children": [ {
                    "name": "logger",
                    "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                } ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"#missing\" is an \"offer\" target from \"#logger\" but it does not appear in \"children\" or \"collections\""
        ),
        test_cml_offer_target_bad_to(
            json!({
                "offer": [ {
                    "protocol": "fuchsia.logger.Log",
                    "from": "#logger",
                    "to": [ "self" ],
                    "as": "fuchsia.logger.SysLog",
                } ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"self\", expected \"#<child-name>\", \"#<collection-name>\", or \"self/<dictionary>\""
        ),
        test_cml_offer_empty_protocols(
            json!({
                "offer": [
                    {
                        "protocol": [],
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "as": "thing"
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a name or nonempty array of names, with unique elements"
        ),
        test_cml_offer_target_equals_from(
            json!({
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/child#meta/child.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                        "to": [ "#child" ],
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "Offer target \"#child\" is same as source"
        ),
        test_cml_offer_target_equals_from_weak(
            json!({
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://fuchsia.com/child#meta/child.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                        "to": [ "#child" ],
                        "dependency": "weak",
                    },
                    {
                        "directory": "data",
                        "from": "#child",
                        "to": [ "#child" ],
                        "dependency": "weak",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_storage_offer_target_equals_from(
            json!({
                "offer": [ {
                    "storage": "minfs",
                    "from": "self",
                    "to": [ "#logger" ],
                } ],
                "children": [ {
                    "name": "logger",
                    "url": "fuchsia-pkg://fuchsia.com/logger#meta/logger.cm",
                } ],
                "capabilities": [ {
                    "storage": "minfs",
                    "from": "#logger",
                    "backing_dir": "minfs-dir",
                    "storage_id": "static_instance_id_or_moniker",
                } ],
            }),
            Err(Error::Validate { err, .. }) if &err == "Storage offer target \"#logger\" is same as source"
        ),
        test_cml_offer_duplicate_target_names(
            json!({
                "offer": [
                    {
                        "protocol": "logger",
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "as": "thing"
                    },
                    {
                        "protocol": "logger",
                        "from": "parent",
                        "to": [ "#scenic" ],
                    },
                    {
                        "directory": "thing",
                        "from": "parent",
                        "to": [ "#echo_server" ],
                    }
                ],
                "children": [
                    {
                        "name": "scenic",
                        "url": "fuchsia-pkg://fuchsia.com/scenic/stable#meta/scenic.cm"
                    },
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"thing\" is a duplicate \"offer\" target capability for \"#echo_server\""
        ),
        test_cml_offer_duplicate_storage_names(
            json!({
                "offer": [
                    {
                        "storage": "cache",
                        "from": "parent",
                        "to": [ "#echo_server" ]
                    },
                    {
                        "storage": "cache",
                        "from": "self",
                        "to": [ "#echo_server" ]
                    }
                ],
                "capabilities": [ {
                    "storage": "cache",
                    "from": "self",
                    "backing_dir": "minfs",
                    "storage_id": "static_instance_id_or_moniker",
                } ],
                "children": [ {
                    "name": "echo_server",
                    "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                } ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"cache\" is a duplicate \"offer\" target capability for \"#echo_server\""
        ),
        // if "as" is specified, only 1 array item is allowed.
        test_cml_offer_bad_as(
            json!({
                "offer": [
                    {
                        "protocol": ["A", "B"],
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "as": "thing"
                    },
                ],
                "children": [
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm"
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"as\" can only be specified when one `protocol` is supplied."
        ),
        test_cml_offer_bad_subdir(
            json!({
                "offer": [
                    {
                        "directory": "index",
                        "subdir": "/",
                        "from": "parent",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_offer_from_self(
            json!({
                "offer": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                    {
                        "protocol": [ "bar_protocol", "baz_protocol" ],
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                    {
                        "directory": "foo_directory",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                    {
                        "runner": "foo_runner",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                    {
                        "resolver": "foo_resolver",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
                "capabilities": [
                    {
                        "protocol": "foo_protocol",
                    },
                    {
                        "protocol": "bar_protocol",
                    },
                    {
                        "protocol": "baz_protocol",
                    },
                    {
                        "directory": "foo_directory",
                        "path": "/dir",
                        "rights": [ "r*" ],
                    },
                    {
                        "runner": "foo_runner",
                        "path": "/svc/fuchsia.sys2.ComponentRunner",
                    },
                    {
                        "resolver": "foo_resolver",
                        "path": "/svc/fuchsia.component.resolution.Resolver",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_offer_service_from_self_missing(
            json!({
                "offer": [
                    {
                        "service": "pkg_service",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "service \"pkg_service\" is offered from self, so it must be declared as a \"service\" in \"capabilities\""
        ),
        test_cml_offer_protocol_from_self_missing(
            json!({
                "offer": [
                    {
                        "protocol": "pkg_protocol",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"pkg_protocol\" is offered from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_offer_protocol_from_self_missing_multiple(
            json!({
                "offer": [
                    {
                        "protocol": [ "foo_protocol", "bar_protocol" ],
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is offered from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_offer_directory_from_self_missing(
            json!({
                "offer": [
                    {
                        "directory": "pkg_directory",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "directory \"pkg_directory\" is offered from self, so it must be declared as a \"directory\" in \"capabilities\""
        ),
        test_cml_offer_runner_from_self_missing(
            json!({
                "offer": [
                    {
                        "runner": "dart",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "runner \"dart\" is offered from self, so it must be declared as a \"runner\" in \"capabilities\""
        ),
        test_cml_offer_resolver_from_self_missing(
            json!({
                "offer": [
                    {
                        "resolver": "pkg_resolver",
                        "from": "self",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "resolver \"pkg_resolver\" is offered from self, so it must be declared as a \"resolver\" in \"capabilities\""
        ),
        test_cml_offer_storage_from_self_missing(
            json!({
                    "offer": [
                        {
                            "storage": "cache",
                            "from": "self",
                            "to": [ "#echo_server" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "echo_server",
                            "url": "fuchsia-pkg://fuchsia.com/echo_server#meta/echo_server.cm",
                        },
                    ],
                }),
            Err(Error::Validate { err, .. }) if &err == "storage \"cache\" is offered from self, so it must be declared as a \"storage\" in \"capabilities\""
        ),
        test_cml_offer_from_self_missing_dictionary(
            json!({
                "offer": [
                    {
                        "protocol": "foo_protocol",
                        "from": "self/dict/inner",
                        "to": [ "#modular" ],
                    },
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"foo_protocol\" is offered from \"self/dict/inner\", so \"dict\" must be declared as a \"dictionary\" in \"capabilities\""
        ),
        test_cml_offer_dependency_on_wrong_type(
            json!({
                    "offer": [ {
                        "resolver": "fuchsia.logger.Log",
                        "from": "parent",
                        "to": [ "#echo_server" ],
                        "dependency": "strong",
                    } ],
                    "children": [ {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://fuchsia.com/echo/stable#meta/echo_server.cm",
                    } ],
                }),
            Err(Error::Validate { err, .. }) if err.starts_with("Dependency can only be provided for")
        ),
        test_cml_offer_dependency_cycle(
            json!({
                    "offer": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": "#a",
                            "to": [ "#b" ],
                            "dependency": "strong"
                        },
                        {
                            "directory": "data",
                            "from": "#b",
                            "to": [ "#c" ],
                        },
                        {
                            "protocol": "ethernet",
                            "from": "#c",
                            "to": [ "#a" ],
                        },
                        {
                            "runner": "elf",
                            "from": "#b",
                            "to": [ "#d" ],
                        },
                        {
                            "resolver": "http",
                            "from": "#d",
                            "to": [ "#b" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "a",
                            "url": "fuchsia-pkg://fuchsia.com/a#meta/a.cm"
                        },
                        {
                            "name": "b",
                            "url": "fuchsia-pkg://fuchsia.com/b#meta/b.cm"
                        },
                        {
                            "name": "c",
                            "url": "fuchsia-pkg://fuchsia.com/b#meta/c.cm"
                        },
                        {
                            "name": "d",
                            "url": "fuchsia-pkg://fuchsia.com/b#meta/d.cm"
                        },
                    ]
                }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: \
                {{#a -> #b -> #c -> #a}, {#b -> #d -> #b}}"
        ),
        test_cml_offer_dependency_cycle_storage(
            json!({
                "capabilities": [
                    {
                        "storage": "data",
                        "from": "#backend",
                        "backing_dir": "blobfs",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "#meta/child.cm",
                    },
                    {
                        "name": "backend",
                        "url": "#meta/backend.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.example.Protocol",
                        "from": "#child",
                        "to": "#backend",
                    },
                    {
                        "storage": "data",
                        "from": "self",
                        "to": "#child",
                    },
                ],
            }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: \
                {{#backend -> #data -> #child -> #backend}}"
        ),
        test_cml_offer_weak_dependency_cycle(
            json!({
                    "offer": [
                        {
                            "protocol": "fuchsia.logger.Log",
                            "from": "#child_a",
                            "to": [ "#child_b" ],
                            "dependency": "weak"
                        },
                        {
                            "service": "fuchsia.service.Test",
                            "from": "#child_a",
                            "to": [ "#child_b" ],
                            "dependency": "weak"
                        },
                        {
                            "directory": "data",
                            "from": "#child_b",
                            "to": [ "#child_a" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "child_a",
                            "url": "fuchsia-pkg://fuchsia.com/child_a#meta/child_a.cm"
                        },
                        {
                            "name": "child_b",
                            "url": "fuchsia-pkg://fuchsia.com/child_b#meta/child_b.cm"
                        },
                    ]
                }),
            Ok(())
        ),

        // children
        test_cml_children(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "on_terminate": "reboot",
                    },
                    {
                        "name": "gmail",
                        "url": "https://www.google.com/gmail",
                        "startup": "eager",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_children_missing_props(
            json!({
                "children": [ {} ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `name`"
        ),
        test_cml_children_duplicate_names(
            json!({
                "children": [
                     {
                         "name": "logger",
                         "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                     },
                     {
                         "name": "logger",
                         "url": "fuchsia-pkg://fuchsia.com/logger/beta#meta/logger.cm"
                     }
                 ]
             }),
             Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"children\" and once in \"children\""
         ),
         test_cml_children_url_ends_in_cml(
            json!({
                "children": [
                     {
                         "name": "logger",
                         "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cml"
                     },
                 ]
             }),
             Err(Error::Validate { err, ..}) if &err == "child URL ends in .cml instead of .cm, which is almost certainly a mistake: fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cml"
         ),
          test_cml_children_bad_startup(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "startup": "zzz",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `zzz`, expected `lazy` or `eager`"
        ),
        test_cml_children_bad_on_terminate(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "on_terminate": "zzz",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `zzz`, expected `none` or `reboot`"
        ),


        test_cml_children_bad_environment(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "environment": "parent",
                    }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"parent\", expected \"#<environment-name>\""
        ),
        test_cml_children_unknown_environment(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "environment": "#foo_env",
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"foo_env\" does not appear in \"environments\""
        ),
        test_cml_children_environment(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                        "environment": "#foo_env",
                    }
                ],
                "environments": [
                    {
                        "name": "foo_env",
                    }
                ]
            }),
            Ok(())
        ),
        test_cml_collections_bad_environment(
            json!({
                "collections": [
                    {
                        "name": "tests",
                        "durability": "transient",
                        "environment": "parent",
                    }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"parent\", expected \"#<environment-name>\""
        ),
        test_cml_collections_unknown_environment(
            json!({
                "collections": [
                    {
                        "name": "tests",
                        "durability": "transient",
                        "environment": "#foo_env",
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"foo_env\" does not appear in \"environments\""
        ),
        test_cml_collections_environment(
            json!({
                "collections": [
                    {
                        "name": "tests",
                        "durability": "transient",
                        "environment": "#foo_env",
                    }
                ],
                "environments": [
                    {
                        "name": "foo_env",
                    }
                ]
            }),
            Ok(())
        ),

        test_cml_environment_timeout(
            json!({
                "environments": [
                    {
                        "name": "foo_env",
                        "__stop_timeout_ms": 10000,
                    }
                ]
            }),
            Ok(())
        ),

        test_cml_environment_bad_timeout(
            json!({
                "environments": [
                    {
                        "name": "foo_env",
                        "__stop_timeout_ms": -3,
                    }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: integer `-3`, expected an unsigned 32-bit integer"
        ),
        test_cml_environment_debug(
            json!({
                "capabilities": [
                    {
                        "protocol": "fuchsia.logger.Log2",
                    },
                ],
                "environments": [
                    {
                        "name": "foo_env",
                        "extends": "realm",
                        "debug": [
                            {
                                "protocol": "fuchsia.module.Module",
                                "from": "#modular",
                            },
                            {
                                "protocol": "fuchsia.logger.OtherLog",
                                "from": "parent",
                            },
                            {
                                "protocol": "fuchsia.logger.Log2",
                                "from": "self",
                            },
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
           Ok(())
        ),
        test_cml_environment_debug_missing_capability(
            json!({
                "environments": [
                    {
                        "name": "foo_env",
                        "extends": "realm",
                        "debug": [
                            {
                                "protocol": "fuchsia.module.Module",
                                "from": "#modular",
                            },
                            {
                                "protocol": "fuchsia.logger.OtherLog",
                                "from": "parent",
                            },
                            {
                                "protocol": "fuchsia.logger.Log2",
                                "from": "self",
                            },
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "protocol \"fuchsia.logger.Log2\" is registered as debug from self, so it must be declared as a \"protocol\" in \"capabilities\""
        ),
        test_cml_environment_invalid_from_child(
            json!({
                "capabilities": [
                    {
                        "protocol": "fuchsia.logger.Log2",
                    },
                ],
                "environments": [
                    {
                        "name": "foo_env",
                        "extends": "realm",
                        "debug": [
                            {
                                "protocol": "fuchsia.module.Module",
                                "from": "#missing",
                            },
                            {
                                "protocol": "fuchsia.logger.OtherLog",
                                "from": "parent",
                            },
                            {
                                "protocol": "fuchsia.logger.Log2",
                                "from": "self",
                            },
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "modular",
                        "url": "fuchsia-pkg://fuchsia.com/modular#meta/modular.cm"
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"debug\" source \"#missing\" does not appear in \"children\" or \"capabilities\""
        ),


        // collections
        test_cml_collections(
            json!({
                "collections": [
                    {
                        "name": "test_single_run_coll",
                        "durability": "single_run"
                    },
                    {
                        "name": "test_transient_coll",
                        "durability": "transient"
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_collections_missing_props(
            json!({
                "collections": [ {} ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `name`"
        ),
        test_cml_collections_duplicate_names(
           json!({
               "collections": [
                    {
                        "name": "duplicate",
                        "durability": "single_run"
                    },
                    {
                        "name": "duplicate",
                        "durability": "transient"
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "identifier \"duplicate\" is defined twice, once in \"collections\" and once in \"collections\""
        ),
        test_cml_collections_bad_durability(
            json!({
                "collections": [
                    {
                        "name": "modular",
                        "durability": "zzz",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `zzz`, expected `transient` or `single_run`"
        ),

        // capabilities
        test_cml_protocol(
            json!({
                "capabilities": [
                    {
                        "protocol": "a",
                        "path": "/minfs",
                    },
                    {
                        "protocol": "b",
                        "path": "/data",
                    },
                    {
                        "protocol": "c",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_protocol_multi(
            json!({
                "capabilities": [
                    {
                        "protocol": ["a", "b", "c"],
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_protocol_multi_invalid_path(
            json!({
                "capabilities": [
                    {
                        "protocol": ["a", "b", "c"],
                        "path": "/minfs",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" can only be specified when one `protocol` is supplied."
        ),
        test_cml_protocol_all_valid_chars(
            json!({
                "capabilities": [
                    {
                        "protocol": "abcdefghijklmnopqrstuvwxyz0123456789_-service",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_directory(
            json!({
                "capabilities": [
                    {
                        "directory": "a",
                        "path": "/minfs",
                        "rights": ["connect"],
                    },
                    {
                        "directory": "b",
                        "path": "/data",
                        "rights": ["connect"],
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_directory_all_valid_chars(
            json!({
                "capabilities": [
                    {
                        "directory": "abcdefghijklmnopqrstuvwxyz0123456789_-service",
                        "path": "/data",
                        "rights": ["connect"],
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_directory_missing_path(
            json!({
                "capabilities": [
                    {
                        "directory": "dir",
                        "rights": ["connect"],
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" should be present with \"directory\""
        ),
        test_cml_directory_missing_rights(
            json!({
                "capabilities": [
                    {
                        "directory": "dir",
                        "path": "/dir",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"rights\" should be present with \"directory\""
        ),
        test_cml_storage(
            json!({
                "capabilities": [
                    {
                        "storage": "a",
                        "from": "#minfs",
                        "backing_dir": "minfs",
                        "storage_id": "static_instance_id",
                    },
                    {
                        "storage": "b",
                        "from": "parent",
                        "backing_dir": "data",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                    {
                        "storage": "c",
                        "from": "self",
                        "backing_dir": "storage",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
                "children": [
                    {
                        "name": "minfs",
                        "url": "fuchsia-pkg://fuchsia.com/minfs/stable#meta/minfs.cm",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_storage_all_valid_chars(
            json!({
                "capabilities": [
                    {
                        "storage": "abcdefghijklmnopqrstuvwxyz0123456789_-storage",
                        "from": "#abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "backing_dir": "example",
                        "storage_id": "static_instance_id_or_moniker",
                    },
                ],
                "children": [
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "url": "https://www.google.com/gmail",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_storage_invalid_from(
            json!({
                    "capabilities": [ {
                        "storage": "minfs",
                        "from": "#missing",
                        "backing_dir": "minfs",
                        "storage_id": "static_instance_id_or_moniker",
                    } ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"capabilities\" source \"#missing\" does not appear in \"children\""
        ),
        test_cml_storage_missing_path_or_backing_dir(
            json!({
                    "capabilities": [ {
                        "storage": "minfs",
                        "from": "self",
                        "storage_id": "static_instance_id_or_moniker",
                    } ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"backing_dir\" should be present with \"storage\""

        ),
        test_cml_storage_missing_storage_id(
            json!({
                    "capabilities": [ {
                        "storage": "minfs",
                        "from": "self",
                        "backing_dir": "storage",
                    }, ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"storage_id\" should be present with \"storage\""
        ),
        test_cml_storage_path(
            json!({
                    "capabilities": [ {
                        "storage": "minfs",
                        "from": "self",
                        "path": "/minfs",
                        "storage_id": "static_instance_id_or_moniker",
                    } ]
                }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" can not be present with \"storage\", use \"backing_dir\""
        ),
        test_cml_runner(
            json!({
                "capabilities": [
                    {
                        "runner": "a",
                        "path": "/minfs",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_runner_all_valid_chars(
            json!({
                "children": [
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-from",
                        "url": "https://www.google.com/gmail"
                    },
                ],
                "capabilities": [
                    {
                        "runner": "abcdefghijklmnopqrstuvwxyz0123456789_-runner",
                        "path": "/example",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_runner_extraneous_from(
            json!({
                "capabilities": [
                    {
                        "runner": "a",
                        "path": "/example",
                        "from": "self",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"from\" should not be present with \"runner\""
        ),
        test_cml_capability_missing_name(
            json!({
                "capabilities": [
                    {
                        "path": "/svc/fuchsia.component.resolution.Resolver",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "`capability` declaration is missing a capability keyword, one of: \"service\", \"protocol\", \"directory\", \"storage\", \"runner\", \"resolver\", \"event_stream\", \"dictionary\", \"config\""
        ),
        test_cml_resolver_missing_path(
            json!({
                "capabilities": [
                    {
                        "resolver": "pkg_resolver",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" should be present with \"resolver\""
        ),
        test_cml_capabilities_extraneous_from(
            json!({
                "capabilities": [
                    {
                        "resolver": "pkg_resolver",
                        "path": "/svc/fuchsia.component.resolution.Resolver",
                        "from": "self",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"from\" should not be present with \"resolver\""
        ),
        test_cml_capabilities_duplicates(
            json!({
                "capabilities": [
                    {
                        "runner": "pkg_resolver",
                        "path": "/svc/fuchsia.component.resolution.Resolver",
                    },
                    {
                        "resolver": "pkg_resolver",
                        "path": "/svc/my-resolver",
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "identifier \"pkg_resolver\" is defined twice, once in \"resolvers\" and once in \"runners\""
        ),

        // environments
        test_cml_environments(
            json!({
                "environments": [
                    {
                        "name": "my_env_a",
                    },
                    {
                        "name": "my_env_b",
                        "extends": "realm",
                    },
                    {
                        "name": "my_env_c",
                        "extends": "none",
                        "__stop_timeout_ms": 8000,
                    },
                ],
            }),
            Ok(())
        ),

        test_invalid_cml_environment_no_stop_timeout(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "none",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err ==
                "'__stop_timeout_ms' must be provided if the environment does not extend \
                another environment"
        ),

        test_cml_environment_invalid_extends(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "some_made_up_string",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `some_made_up_string`, expected `realm` or `none`"
        ),
        test_cml_environment_missing_props(
            json!({
                "environments": [ {} ]
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `name`"
        ),

        test_cml_environment_with_runners(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "dart",
                                "from": "parent",
                            }
                        ]
                    }
                ],
            }),
            Ok(())
        ),
        test_cml_environment_with_runners_alias(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "dart",
                                "from": "parent",
                                "as": "my-dart",
                            }
                        ]
                    }
                ],
            }),
            Ok(())
        ),
        test_cml_environment_with_runners_missing(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "dart",
                                "from": "self",
                            }
                        ]
                    }
                ],
                "capabilities": [
                     {
                         "runner": "dart",
                         "path": "/svc/fuchsia.component.Runner",
                     }
                ],
            }),
            Ok(())
        ),
        test_cml_environment_with_runners_bad_name(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "elf",
                                "from": "parent",
                                "as": "#elf",
                            }
                        ]
                    }
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"#elf\", expected a \
            name that consists of [A-Za-z0-9_.-] and starts with [A-Za-z0-9_]"
        ),
        test_cml_environment_with_runners_duplicate_name(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "dart",
                                "from": "parent",
                            },
                            {
                                "runner": "other-dart",
                                "from": "parent",
                                "as": "dart",
                            }
                        ]
                    }
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "Duplicate runners registered under name \"dart\": \"other-dart\" and \"dart\"."
        ),
        test_cml_environment_with_runner_from_missing_child(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "elf",
                                "from": "#missing_child",
                            }
                        ]
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"elf\" runner source \"#missing_child\" does not appear in \"children\""
        ),
        test_cml_environment_with_runner_cycle(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "runners": [
                            {
                                "runner": "elf",
                                "from": "#child",
                                "as": "my-elf",
                            }
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://child",
                        "environment": "#my_env",
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err ==
                    "Strong dependency cycles were found. Break the cycle by removing a \
                    dependency or marking an offer as weak. Cycles: \
                    {{#child -> #my_env -> #child}}"
        ),
        test_cml_environment_with_resolvers(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "parent",
                                "scheme": "fuchsia-pkg",
                            }
                        ]
                    }
                ],
            }),
            Ok(())
        ),
        test_cml_environment_with_resolvers_bad_scheme(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "parent",
                                "scheme": "9scheme",
                            }
                        ]
                    }
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"9scheme\", expected a valid URL scheme"
        ),
        test_cml_environment_with_resolvers_duplicate_scheme(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "parent",
                                "scheme": "fuchsia-pkg",
                            },
                            {
                                "resolver": "base_resolver",
                                "from": "parent",
                                "scheme": "fuchsia-pkg",
                            }
                        ]
                    }
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "scheme \"fuchsia-pkg\" for resolver \"base_resolver\" is already registered; previously registered to resolver \"pkg_resolver\"."
        ),
        test_cml_environment_with_resolver_from_missing_child(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "#missing_child",
                                "scheme": "fuchsia-pkg",
                            }
                        ]
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"pkg_resolver\" resolver source \"#missing_child\" does not appear in \"children\""
        ),
        test_cml_environment_with_resolver_cycle(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "#child",
                                "scheme": "fuchsia-pkg",
                            }
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://child",
                        "environment": "#my_env",
                    }
                ]
            }),
            Err(Error::Validate { err, .. }) if &err ==
                    "Strong dependency cycles were found. Break the cycle by removing a \
                    dependency or marking an offer as weak. \
                    Cycles: {{#child -> #my_env -> #child}}"
        ),
        test_cml_environment_with_cycle_multiple_components(
            json!({
                "environments": [
                    {
                        "name": "my_env",
                        "extends": "realm",
                        "resolvers": [
                            {
                                "resolver": "pkg_resolver",
                                "from": "#b",
                                "scheme": "fuchsia-pkg",
                            }
                        ]
                    }
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "fuchsia-pkg://a",
                        "environment": "#my_env",
                    },
                    {
                        "name": "b",
                        "url": "fuchsia-pkg://b",
                    }
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.logger.Log",
                        "from": "#a",
                        "to": [ "#b" ],
                        "dependency": "strong"
                    },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a dependency \
                or marking an offer as weak. \
                Cycles: {{#a -> #b -> #my_env -> #a}}"
        ),

        // facets
        test_cml_facets(
            json!({
                "facets": {
                    "metadata": {
                        "title": "foo",
                        "authors": [ "me", "you" ],
                        "year": 2018
                    }
                }
            }),
            Ok(())
        ),
        test_cml_facets_wrong_type(
            json!({
                "facets": 55
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: integer `55`, expected a map"
        ),

        // constraints
        test_cml_rights_all(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": ["connect", "enumerate", "read_bytes", "write_bytes",
                               "execute", "update_attributes", "get_attributes", "traverse",
                               "modify_directory"],
                  },
                ]
            }),
            Ok(())
        ),
        test_cml_rights_invalid(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": ["cAnnect", "enumerate"],
                  },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `cAnnect`, expected one of `connect`, `enumerate`, `execute`, `get_attributes`, `modify_directory`, `read_bytes`, `traverse`, `update_attributes`, `write_bytes`, `r*`, `w*`, `x*`, `rw*`, `rx*`"
        ),
        test_cml_rights_duplicate(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": ["connect", "connect"],
                  },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: array with duplicate element, expected a nonempty array of rights, with unique elements"
        ),
        test_cml_rights_empty(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": [],
                  },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a nonempty array of rights, with unique elements"
        ),
        test_cml_rights_alias_star_expansion(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "rights": ["r*"],
                    "path": "/mydir",
                  },
                ]
            }),
            Ok(())
        ),
        test_cml_rights_alias_star_expansion_with_longform(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "rights": ["w*", "read_bytes"],
                    "path": "/mydir",
                  },
                ]
            }),
            Ok(())
        ),
        test_cml_rights_alias_star_expansion_with_longform_collision(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": ["r*", "read_bytes"],
                  },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"read_bytes\" is duplicated in the rights clause."
        ),
        test_cml_rights_alias_star_expansion_collision(
            json!({
                "use": [
                  {
                    "directory": "mydir",
                    "path": "/mydir",
                    "rights": ["w*", "x*"],
                  },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "\"x*\" is duplicated in the rights clause."
        ),
        test_cml_rights_use_invalid(
            json!({
                "use": [
                  { "directory": "mydir", "path": "/mydir" },
                ]
            }),
            Err(Error::Validate { err, .. }) if &err == "This use statement requires a `rights` field. Refer to: https://fuchsia.dev/go/components/directory#consumer."
        ),

        test_cml_path(
            json!({
                "capabilities": [
                    {
                        "protocol": "foo",
                        "path": "/foo/in.-_/Bar",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_path_invalid_empty(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path": "" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH bytes in length"
        ),
        test_cml_path_invalid_root(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path": "/" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/\", expected a path with leading `/` and non-empty segments, where each segment is no more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., and cannot contain embedded NULs"
        ),
        test_cml_path_invalid_absolute_is_relative(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path": "foo/bar" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"foo/bar\", expected a path with leading `/` and non-empty segments, where each segment is no more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., and cannot contain embedded NULs"
        ),
        test_cml_path_invalid_trailing(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path":"/foo/bar/" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/foo/bar/\", expected a path with leading `/` and non-empty segments, where each segment is no more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., and cannot contain embedded NULs"
        ),
        test_cml_path_too_long(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path": format!("/{}", "a".repeat(4095)) },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 4096, expected a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH bytes in length"
        ),
        test_cml_path_invalid_segment(
            json!({
                "capabilities": [
                    { "protocol": "foo", "path": "/foo/../bar" },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/foo/../bar\", expected a path with leading `/` and non-empty segments, where each segment is no more than fuchsia.io/MAX_NAME_LENGTH bytes in length, cannot be . or .., and cannot contain embedded NULs"
        ),
        test_cml_relative_path(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": "Baz/Bar",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_relative_path_invalid_empty(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": "",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH characters in length"
        ),
        test_cml_relative_path_invalid_root(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": "/",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_relative_path_invalid_absolute(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": "/bar",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/bar\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_relative_path_invalid_trailing(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": "bar/",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bar/\", expected a path with no leading `/` and non-empty segments"
        ),
        test_cml_relative_path_too_long(
            json!({
                "use": [
                    {
                        "directory": "foo",
                        "path": "/foo",
                        "rights": ["r*"],
                        "subdir": format!("{}", "a".repeat(4096)),
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 4096, expected a non-empty path no more than fuchsia.io/MAX_PATH_LENGTH characters in length"
        ),
        test_cml_relative_ref_too_long(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.logger.Log",
                        "from": &format!("#{}", "a".repeat(256)),
                    },
                ],
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 257, expected one or an array of \"framework\", \"self\", \"#<child-name>\", or a dictionary path"
        ),
        test_cml_dictionary_ref_invalid_root(
            json!({
                "use": [
                    {
                        "protocol": "a",
                        "from": "bad/a",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"bad/a\", expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
        ),
        test_cml_dictionary_ref_invalid_path(
            json!({
                "use": [
                    {
                        "protocol": "a",
                        "from": "parent//a",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"parent//a\", expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
        ),
        test_cml_dictionary_ref_too_long(
            json!({
                "use": [
                    {
                        "protocol": "a",
                        "from": format!("parent/{}", "a".repeat(4089)),
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 4096, expected \"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
        ),
        test_cml_capability_name(
            json!({
                "use": [
                    {
                        "protocol": "abcdefghijklmnopqrstuvwxyz0123456789_-.",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_capability_name_invalid(
            json!({
                "use": [
                    {
                        "protocol": "/bad",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/bad\", expected a name or nonempty array of names, with unique elements"
        ),
        test_cml_child_name(
            json!({
                "children": [
                    {
                        "name": "abcdefghijklmnopqrstuvwxyz0123456789_-.",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_child_name_invalid(
            json!({
                "children": [
                    {
                        "name": "/bad",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"/bad\", expected a \
            name that consists of [A-Za-z0-9_.-] and starts with [A-Za-z0-9_]"
        ),
        test_cml_child_name_too_long(
            json!({
                "children": [
                    {
                        "name": "a".repeat(256),
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                    }
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 256, expected a non-empty name no more than 255 characters in length"
        ),
        test_cml_url(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "my+awesome-scheme.2://abc123!@$%.com",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_url_host_pound_invalid(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "my+awesome-scheme.2://abc123!@#$%.com",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"my+awesome-scheme.2://abc123!@#$%.com\", expected a valid URL"
        ),
        test_cml_url_invalid(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg",
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"fuchsia-pkg\", expected a valid URL"
        ),
        test_cml_url_too_long(
            json!({
                "children": [
                    {
                        "name": "logger",
                        "url": &format!("fuchsia-pkg://{}", "a".repeat(4083)),
                    },
                ]
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 4097, expected a non-empty URL no more than 4096 characters in length"
        ),
        test_cml_duplicate_identifiers_children_collection(
           json!({
               "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    }
               ],
               "collections": [
                   {
                       "name": "logger",
                       "durability": "transient"
                   }
               ]
           }),
           Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"collections\" and once in \"children\""
        ),
        test_cml_duplicate_identifiers_children_storage(
           json!({
               "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    }
               ],
               "capabilities": [
                    {
                        "storage": "logger",
                        "path": "/logs",
                        "from": "parent"
                    }
                ]
           }),
           Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"storage\" and once in \"children\""
        ),
        test_cml_duplicate_identifiers_collection_storage(
           json!({
               "collections": [
                    {
                        "name": "logger",
                        "durability": "transient"
                    }
                ],
                "capabilities": [
                    {
                        "storage": "logger",
                        "path": "/logs",
                        "from": "parent"
                    }
                ]
           }),
           Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"storage\" and once in \"collections\""
        ),
        test_cml_duplicate_identifiers_children_runners(
           json!({
               "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                    }
               ],
               "capabilities": [
                    {
                        "runner": "logger",
                        "from": "parent"
                    }
                ]
           }),
           Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"runners\" and once in \"children\""
        ),
        test_cml_duplicate_identifiers_environments(
            json!({
                "children": [
                     {
                         "name": "logger",
                         "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm"
                     }
                ],
                "environments": [
                     {
                         "name": "logger",
                     }
                 ]
            }),
            Err(Error::Validate { err, .. }) if &err == "identifier \"logger\" is defined twice, once in \"environments\" and once in \"children\""
        ),

        // deny unknown fields
        test_deny_unknown_fields(
            json!(
                {
                    "program": {
                        "runner": "elf",
                        "binary": "bin/app",
                    },
                    "unknown_field": {},
                }
            ),
            Err(Error::Parse { err, .. }) if err.starts_with("unknown field `unknown_field`, expected one of ")
        ),
        test_offer_source_availability_unknown(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "to": "#foo",
                        "availability": "optional",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Ok(())
        ),
        test_offer_source_availability_required(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "to": "#foo",
                        "source_availability": "required",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#bar\" does not appear in \"children\" or \"capabilities\""
        ),
        test_offer_source_availability_omitted(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "to": "#foo",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" source \"#bar\" does not appear in \"children\" or \"capabilities\""
        ),
        test_cml_use_invalid_availability(
            json!({
                "use": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "availability": "same_as_target",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"availability: same_as_target\" cannot be used with use declarations"
        ),
        test_offer_source_void_availability_required(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "void",
                        "to": "#foo",
                        "availability": "required",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with a source of \"void\" must have an availability of \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"void\""
        ),
        test_offer_source_void_availability_same_as_target(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "void",
                        "to": "#foo",
                        "availability": "same_as_target",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with a source of \"void\" must have an availability of \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"void\""
        ),
        test_offer_source_missing_availability_required(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "to": "#foo",
                        "availability": "required",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with an intentionally missing source must have an availability that is either unset or \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"#bar\""
        ),
        test_offer_source_missing_availability_same_as_target(
            json!({
                "children": [
                    {
                        "name": "foo",
                        "url": "fuchsia-pkg://foo.com/foo#meta/foo.cm"
                    },
                ],
                "offer": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "to": "#foo",
                        "availability": "same_as_target",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with an intentionally missing source must have an availability that is either unset or \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"#bar\""
        ),
        test_expose_source_availability_unknown(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "availability": "optional",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Ok(())
        ),
        test_expose_source_availability_required(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "source_availability": "required",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#bar\" does not appear in \"children\" or \"capabilities\""
        ),
        test_expose_source_availability_omitted(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"expose\" source \"#bar\" does not appear in \"children\" or \"capabilities\""
        ),
        test_expose_source_void_availability_required(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "void",
                        "availability": "required",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with a source of \"void\" must have an availability of \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"void\""
        ),
        test_expose_source_void_availability_same_as_target(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "void",
                        "availability": "same_as_target",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with a source of \"void\" must have an availability of \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"void\""
        ),
        test_expose_source_missing_availability_required(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "availability": "required",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with an intentionally missing source must have an availability that is either unset or \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"#bar\""
        ),
        test_expose_source_missing_availability_same_as_target(
            json!({
                "expose": [
                    {
                        "protocol": "fuchsia.examples.Echo",
                        "from": "#bar",
                        "availability": "same_as_target",
                        "source_availability": "unknown",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "capabilities with an intentionally missing source must have an availability that is either unset or \"optional\", capabilities: \"fuchsia.examples.Echo\", from: \"#bar\""
        ),
    }

    // Tests for services.
    test_validate_cml! {
        test_cml_validate_use_service(
            json!({
                "use": [
                    { "service": "CoolFonts", "path": "/svc/fuchsia.fonts.Provider" },
                    { "service": "fuchsia.component.Realm", "from": "framework" },
                ],
            }),
            Ok(())
        ),
        test_cml_use_invalid_from_with_service(
            json!({
                "use": [ { "service": "foo", "from": "debug" } ]
            }),
            Err(Error::Validate { err, .. }) if &err == "only \"protocol\" supports source from \"debug\""
        ),
        test_cml_validate_offer_service(
            json!({
                "offer": [
                    {
                        "service": "fuchsia.logger.Log",
                        "from": "#logger",
                        "to": [ "#echo_server", "#modular" ],
                        "as": "fuchsia.logger.SysLog"
                    },
                    {
                        "service": "fuchsia.fonts.Provider",
                        "from": "parent",
                        "to": [ "#echo_server" ]
                    },
                    {
                        "service": "fuchsia.net.Netstack",
                        "from": "self",
                        "to": [ "#echo_server" ]
                    },
                ],
                "children": [
                    {
                        "name": "logger",
                        "url": "fuchsia-pkg://logger",
                    },
                    {
                        "name": "echo_server",
                        "url": "fuchsia-pkg://echo_server",
                    }
                ],
                "collections": [
                    {
                        "name": "modular",
                        "durability": "transient",
                    },
                ],
                "capabilities": [
                    { "service": "fuchsia.net.Netstack" },
                ],
            }),
            Ok(())
        ),
        test_cml_validate_expose_service(
            json!(
                {
                    "expose": [
                        {
                            "service": "fuchsia.fonts.Provider",
                            "from": "self",
                        },
                        {
                            "service": "fuchsia.logger.Log",
                            "from": "#logger",
                            "as": "logger"
                        },
                    ],
                    "capabilities": [
                        { "service": "fuchsia.fonts.Provider" },
                    ],
                    "children": [
                        {
                            "name": "logger",
                            "url": "fuchsia-pkg://logger",
                        },
                    ]
                }
            ),
            Ok(())
        ),
        test_cml_validate_expose_service_multi_source(
            json!(
                {
                    "expose": [
                        {
                            "service": "fuchsia.my.Service",
                            "from": [ "self", "#a" ],
                        },
                        {
                            "service": "fuchsia.my.Service",
                            "from": "#coll",
                        },
                    ],
                    "capabilities": [
                        { "service": "fuchsia.my.Service" },
                    ],
                    "children": [
                        {
                            "name": "a",
                            "url": "fuchsia-pkg://a",
                        },
                    ],
                    "collections": [
                        {
                            "name": "coll",
                            "durability": "transient",
                        },
                    ],
                }
            ),
            Ok(())
        ),
        test_cml_validate_offer_service_multi_source(
            json!(
                {
                    "offer": [
                        {
                            "service": "fuchsia.my.Service",
                            "from": [ "self", "parent" ],
                            "to": "#b",
                        },
                        {
                            "service": "fuchsia.my.Service",
                            "from": [ "#a", "#coll" ],
                            "to": "#b",
                        },
                    ],
                    "capabilities": [
                        { "service": "fuchsia.my.Service" },
                    ],
                    "children": [
                        {
                            "name": "a",
                            "url": "fuchsia-pkg://a",
                        },
                        {
                            "name": "b",
                            "url": "fuchsia-pkg://b",
                        },
                    ],
                    "collections": [
                        {
                            "name": "coll",
                            "durability": "transient",
                        },
                    ],
                }
            ),
            Ok(())
        ),
        test_cml_service(
            json!({
                "capabilities": [
                    {
                        "protocol": "a",
                        "path": "/minfs",
                    },
                    {
                        "protocol": "b",
                        "path": "/data",
                    },
                    {
                        "protocol": "c",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_service_multi(
            json!({
                "capabilities": [
                    {
                        "service": ["a", "b", "c"],
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_service_multi_invalid_path(
            json!({
                "capabilities": [
                    {
                        "service": ["a", "b", "c"],
                        "path": "/minfs",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"path\" can only be specified when one `service` is supplied."
        ),
        test_cml_service_all_valid_chars(
            json!({
                "capabilities": [
                    {
                        "service": "abcdefghijklmnopqrstuvwxyz0123456789_-service",
                    },
                ],
            }),
            Ok(())
        ),
    }

    // Tests structured config
    test_validate_cml_with_feature! { FeatureSet::from(vec![]), {
        test_cml_configs(
            json!({
                "config": {
                    "verbosity": {
                        "type": "string",
                        "max_size": 20,
                    },
                    "timeout": { "type": "uint64" },
                    "tags": {
                        "type": "vector",
                        "max_count": 10,
                        "element": {
                            "type": "string",
                            "max_size": 50
                        }
                    }
                }
            }),
            Ok(())
        ),

        test_cml_configs_not_object(
            json!({
                "config": "abcd"
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: string \"abcd\", expected a map"
        ),

        test_cml_configs_empty(
            json!({
                "config": {
                }
            }),
            Err(Error::Validate { err, .. }) if &err == "'config' section is empty"
        ),

        test_cml_configs_bad_type(
            json!({
                "config": {
                    "verbosity": 123456
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: integer `123456`, expected internally tagged enum ConfigValueType"
        ),

        test_cml_configs_unknown_type(
            json!({
                "config": {
                    "verbosity": {
                        "type": "foo"
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "unknown variant `foo`, expected one of `bool`, `uint8`, `uint16`, `uint32`, `uint64`, `int8`, `int16`, `int32`, `int64`, `string`, `vector`"
        ),

        test_cml_configs_no_max_count_vector(
            json!({
                "config": {
                    "tags": {
                        "type": "vector",
                        "element": {
                            "type": "string",
                            "max_size": 50,
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `max_count`"
        ),

        test_cml_configs_no_max_size_string(
            json!({
                "config": {
                    "verbosity": {
                        "type": "string",
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `max_size`"
        ),

        test_cml_configs_no_max_size_string_vector(
            json!({
                "config": {
                    "tags": {
                        "type": "vector",
                        "max_count": 10,
                        "element": {
                            "type": "string",
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "missing field `max_size`"
        ),

        test_cml_configs_empty_key(
            json!({
                "config": {
                    "": {
                        "type": "bool"
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 0, expected a non-empty name no more than 64 characters in length"
        ),

        test_cml_configs_too_long_key(
            json!({
                "config": {
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {
                        "type": "bool"
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid length 74, expected a non-empty name no more than 64 characters in length"
        ),
        test_cml_configs_key_starts_with_number(
            json!({
                "config": {
                    "8abcd": { "type": "uint8" }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"8abcd\", expected a name which must start with a letter, can contain letters, numbers, and underscores, but cannot end with an underscore"
        ),

        test_cml_configs_key_ends_with_underscore(
            json!({
                "config": {
                    "abcd_": { "type": "uint8" }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"abcd_\", expected a name which must start with a letter, can contain letters, numbers, and underscores, but cannot end with an underscore"
        ),

        test_cml_configs_capitals_in_key(
            json!({
                "config": {
                    "ABCD": { "type": "uint8" }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"ABCD\", expected a name which must start with a letter, can contain letters, numbers, and underscores, but cannot end with an underscore"
        ),

        test_cml_configs_special_chars_in_key(
            json!({
                "config": {
                    "!@#$": { "type": "uint8" }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"!@#$\", expected a name which must start with a letter, can contain letters, numbers, and underscores, but cannot end with an underscore"
        ),

        test_cml_configs_dashes_in_key(
            json!({
                "config": {
                    "abcd-efgh": { "type": "uint8" }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: string \"abcd-efgh\", expected a name which must start with a letter, can contain letters, numbers, and underscores, but cannot end with an underscore"
        ),

        test_cml_configs_bad_max_size_string(
            json!({
                "config": {
                    "verbosity": {
                        "type": "string",
                        "max_size": "abcd"
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: string \"abcd\", expected a nonzero u32"
        ),

        test_cml_configs_zero_max_size_string(
            json!({
                "config": {
                    "verbosity": {
                        "type": "string",
                        "max_size": 0
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: integer `0`, expected a nonzero u32"
        ),

        test_cml_configs_bad_max_count_on_vector(
            json!({
                "config": {
                    "toggles": {
                        "type": "vector",
                        "max_count": "abcd",
                        "element": {
                            "type": "bool"
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: string \"abcd\", expected a nonzero u32"
        ),

        test_cml_configs_zero_max_count_on_vector(
            json!({
                "config": {
                    "toggles": {
                        "type": "vector",
                        "max_count": 0,
                        "element": {
                            "type": "bool"
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: integer `0`, expected a nonzero u32"
        ),

        test_cml_configs_bad_max_size_string_vector(
            json!({
                "config": {
                    "toggles": {
                        "type": "vector",
                        "max_count": 100,
                        "element": {
                            "type": "string",
                            "max_size": "abcd"
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid type: string \"abcd\", expected a nonzero u32"
        ),

        test_cml_configs_zero_max_size_string_vector(
            json!({
                "config": {
                    "toggles": {
                        "type": "vector",
                        "max_count": 100,
                        "element": {
                            "type": "string",
                            "max_size": 0
                        }
                    }
                }
            }),
            Err(Error::Parse { err, .. }) if &err == "invalid value: integer `0`, expected a nonzero u32"
        ),
    }}

    // Tests the use of `allow_long_names` when the "AllowLongNames" feature is set.
    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::AllowLongNames]), {
        test_cml_validate_set_allow_long_names_true(
            json!({
                "collections": [
                    {
                        "name": "foo",
                        "durability": "transient",
                        "allow_long_names": true
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_validate_set_allow_long_names_false(
            json!({
                "collections": [
                    {
                        "name": "foo",
                        "durability": "transient",
                        "allow_long_names": false
                    },
                ],
            }),
            Ok(())
        ),
    }}

    // Tests that the use of `allow_long_names` fails when the "AllowLongNames"
    // feature is not set.
    test_validate_cml! {
        test_cml_allow_long_names_without_feature(
            json!({
                "collections": [
                    {
                        "name": "foo",
                        "durability": "transient",
                        "allow_long_names": true
                    },
                ],
            }),
            Err(Error::RestrictedFeature(s)) if s == "allow_long_names"
        ),
    }

    // Tests validate_facets function without the feature set
    test_validate_cml! {
        test_valid_empty_facets(
            json!({
                "facets": {}
            }),
            Ok(())
        ),

        test_invalid_empty_facets(
            json!({
                "facets": ""
            }),
            Err(err) if err.to_string().contains("invalid type: string")
        ),
        test_valid_empty_fuchsia_test_facet(
            json!({
                "facets": {TEST_FACET_KEY: {}}
            }),
            Ok(())
        ),

        test_valid_allowed_pkg_without_feature(
            json!({
                "facets": {
                    TEST_TYPE_FACET_KEY: "some_realm",
                    TEST_FACET_KEY: {
                        TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: [ "some_pkg" ]
                    }
                }
            }),
            Ok(())
        ),
    }

    // Tests validate_facets function with the RestrictTestTypeInFacet enabled.
    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::RestrictTestTypeInFacet]), {
        test_valid_empty_facets_with_test_type_feature_enabled(
            json!({
                "facets": {}
            }),
            Ok(())
        ),
        test_valid_empty_fuchsia_test_facet_with_test_type_feature_enabled(
            json!({
                "facets": {TEST_FACET_KEY: {}}
            }),
            Ok(())
        ),

        test_invalid_test_type_with_feature_enabled(
            json!({
                "facets": {
                    TEST_FACET_KEY: {
                        TEST_TYPE_FACET_KEY: "some_realm",
                    }
                }
            }),
            Err(err) if err.to_string().contains(TEST_TYPE_FACET_KEY)
        ),
    }}

    // Tests validate_facets function with the EnableAllowNonHermeticPackagesFeature disabled.
    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::AllowNonHermeticPackages]), {
        test_valid_empty_facets_with_feature_disabled(
            json!({
                "facets": {}
            }),
            Ok(())
        ),
        test_valid_empty_fuchsia_test_facet_with_feature_disabled(
            json!({
                "facets": {TEST_FACET_KEY: {}}
            }),
            Ok(())
        ),

        test_valid_allowed_pkg_with_feature_disabled(
            json!({
                "facets": {
                    TEST_FACET_KEY: {
                        TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: [ "some_pkg" ]
                    }
                }
            }),
            Ok(())
        ),
    }}

    // Tests validate_facets function with the EnableAllowNonHermeticPackagesFeature enabled.
    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::EnableAllowNonHermeticPackagesFeature]), {
        test_valid_empty_facets_with_feature_enabled(
            json!({
                "facets": {}
            }),
            Ok(())
        ),
        test_valid_empty_fuchsia_test_facet_with_feature_enabled(
            json!({
                "facets": {TEST_FACET_KEY: {}}
            }),
            Ok(())
        ),

        test_invalid_allowed_pkg_with_feature_enabled(
            json!({
                "facets": {
                    TEST_FACET_KEY: {
                        TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: [ "some_pkg" ]
                    }
                }
            }),
            Err(err) if err.to_string().contains(&Feature::AllowNonHermeticPackages.to_string())
        ),
    }}

    // Tests validate_facets function with the feature enabled and allowed pkg feature set.
    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::EnableAllowNonHermeticPackagesFeature, Feature::AllowNonHermeticPackages]), {
        test_invalid_empty_facets_with_feature_enabled(
            json!({
                "facets": {}
            }),
            Err(err) if err.to_string().contains(&Feature::AllowNonHermeticPackages.to_string())
        ),
        test_invalid_empty_fuchsia_test_facet_with_feature_enabled(
            json!({
                "facets": {TEST_FACET_KEY: {}}
            }),
            Err(err) if err.to_string().contains(&Feature::AllowNonHermeticPackages.to_string())
        ),

        test_valid_allowed_pkg_with_feature_enabled(
            json!({
                "facets": {
                    TEST_FACET_KEY: {
                        TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: [ "some_pkg" ]
                    }
                }
            }),
            Ok(())
        ),
    }}

    test_validate_cml_with_feature! { FeatureSet::from(vec![Feature::DynamicDictionaries]), {
        test_cml_offer_to_dictionary_unsupported(
            json!({
                "offer": [
                    {
                        "event_stream": "p",
                        "from": "parent",
                        "to": "self/dict",
                    },
                ],
                "capabilities": [
                    {
                        "dictionary": "dict",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" to dictionary \
            \"self/dict\" for \"event_stream\" but dictionaries do not support this type yet."
        ),
        test_cml_dictionary_ref(
            json!({
                "use": [
                    {
                        "protocol": "a",
                        "from": "parent/a",
                    },
                    {
                        "protocol": "b",
                        "from": "#child/a/b",
                    },
                    {
                        "protocol": "c",
                        "from": "self/a/b/c",
                    },
                ],
                "capabilities": [
                    {
                        "dictionary": "a",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://child",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_use_dictionary_disallowed(
            json!({
                "use": [
                    {
                        "dictionary": "dict",
                    },
                ],
            }),
            Err(Error::Parse { err, .. }) if err.starts_with("unknown field `dictionary`")
        ),
        test_cml_expose_dictionary_from_self(
            json!({
                "expose": [
                    {
                        "dictionary": "foo_dictionary",
                        "from": "self",
                    },
                ],
                "capabilities": [
                    {
                        "dictionary": "foo_dictionary",
                    },
                ]
            }),
            Ok(())
        ),
        test_cml_offer_to_dictionary_duplicate(
            json!({
                "offer": [
                    {
                        "protocol": "p",
                        "from": "parent",
                        "to": "self/dict",
                    },
                    {
                        "protocol": "p",
                        "from": "#child",
                        "to": "self/dict",
                    },
                ],
                "capabilities": [
                    {
                        "dictionary": "dict",
                    },
                ],
                "children": [
                    {
                        "name": "child",
                        "url": "fuchsia-pkg://child",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"p\" is a duplicate \"offer\" target capability for \"self/dict\""
        ),
        test_cml_offer_to_dictionary_dynamic(
            json!({
                "offer": [
                    {
                        "protocol": "p",
                        "from": "parent",
                        "to": "self/dict",
                    },
                ],
                "capabilities": [
                    {
                        "dictionary": "dict",
                        "path": "/out/dir",
                    },
                ],
            }),
            Err(Error::Validate { err, .. }) if &err == "\"offer\" has dictionary target \"self/dict\" but \"dict\" sets \"path\". Therefore, it is a dynamic dictionary that does not allow offers into it."
        ),
        test_cml_offer_dependency_cycle_from_dictionary(
            json!({
                    "offer": [
                        {
                            "protocol": "1",
                            "from": "#a/in/dict",
                            "to": [ "#b" ],
                            "dependency": "strong"
                        },
                        {
                            "directory": "2",
                            "from": "#b/in/dict",
                            "to": [ "#a" ],
                        },
                    ],
                    "children": [
                        {
                            "name": "a",
                            "url": "fuchsia-pkg://fuchsia.com/a#meta/a.cm"
                        },
                        {
                            "name": "b",
                            "url": "fuchsia-pkg://fuchsia.com/b#meta/b.cm"
                        },
                    ]
                }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: \
                {{#a -> #b -> #a}}"
        ),
        test_cml_offer_dependency_cycle_with_dictionary(
            json!({
                "capabilities": [
                    {
                        "dictionary": "dict",
                    },
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "#meta/a.cm",
                    },
                    {
                        "name": "b",
                        "url": "#meta/b.cm",
                    },
                ],
                "offer": [
                    {
                        "dictionary": "dict",
                        "from": "self",
                        "to": "#a",
                    },
                    {
                        "protocol": "1",
                        "from": "#b",
                        "to": "self/dict",
                    },
                    {
                        "protocol": "2",
                        "from": "#a",
                        "to": "#b",
                    },
                ],
            }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: {{#a -> #b -> #dict -> #a}}"
        ),
        test_cml_offer_dependency_cycle_with_dictionary_indirect(
            json!({
                "capabilities": [
                    {
                        "dictionary": "dict",
                    },
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "#meta/a.cm",
                    },
                    {
                        "name": "b",
                        "url": "#meta/b.cm",
                    },
                ],
                "offer": [
                    {
                        "protocol": "3",
                        "from": "self/dict",
                        "to": "#a",
                    },
                    {
                        "protocol": "1",
                        "from": "#b",
                        "to": "self/dict",
                    },
                    {
                        "protocol": "2",
                        "from": "#a",
                        "to": "#b",
                    },
                ],
            }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: {{#a -> #b -> #dict -> #a}}"
        ),
        test_cml_use_dependency_cycle_with_dictionary(
            json!({
                "capabilities": [
                    {
                        "protocol": "1",
                    },
                    {
                        "dictionary": "dict",
                    },
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "#meta/a.cm",
                    },
                ],
                "use": [
                    {
                        "protocol": "2",
                        "from": "#a",
                    },
                ],
                "offer": [
                    {
                        "dictionary": "dict",
                        "from": "self",
                        "to": "#a",
                    },
                    {
                        "protocol": "1",
                        "from": "self",
                        "to": "self/dict",
                    },
                ],
            }),
            Err(Error::Validate {
                err,
                ..
            }) if &err ==
                "Strong dependency cycles were found. Break the cycle by removing a \
                dependency or marking an offer as weak. Cycles: {{#a -> self -> #dict -> #a}}"
        ),
    }}

    // Tests that offering and exposing service capabilities to the same target and target name is
    // allowed.
    test_validate_cml! {
        test_cml_aggregate_expose(
            json!({
                "expose": [
                    {
                        "service": "fuchsia.foo.Bar",
                        "from": ["#a", "#b"],
                    },
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "fuchsia-pkg://fuchsia.com/a#meta/a.cm",
                    },
                    {
                        "name": "b",
                        "url": "fuchsia-pkg://fuchsia.com/b#meta/b.cm",
                    },
                ],
            }),
            Ok(())
        ),
        test_cml_aggregate_offer(
            json!({
                "offer": [
                    {
                        "service": "fuchsia.foo.Bar",
                        "from": ["#a", "#b"],
                        "to": "#target",
                    },
                ],
                "children": [
                    {
                        "name": "a",
                        "url": "fuchsia-pkg://fuchsia.com/a#meta/a.cm",
                    },
                    {
                        "name": "b",
                        "url": "fuchsia-pkg://fuchsia.com/b#meta/b.cm",
                    },
                    {
                        "name": "target",
                        "url": "fuchsia-pkg://fuchsia.com/target#meta/target.cm",
                    },
                ],
            }),
            Ok(())
        ),
    }

    use crate::translate::test_util::must_parse_cml;
    use crate::translate::{compile, CompileOptions};

    #[test]
    fn test_cml_use_bad_config_from_self() {
        let input = must_parse_cml!({
        "use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "from": "self",
            },
        ],
        });

        let options = CompileOptions::new();
        assert_matches!(compile(&input, options), Err(Error::Validate { .. }));
    }

    // Tests for config capabilities
    test_validate_cml! {
        a_test_cml_use_config(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
            },
        ],}),
        Ok(())
        ),
        test_cml_use_config_good_vector(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "vector",
                "element": { "type": "bool"},
                "max_count": 1,
            },
        ],}),
        Ok(())
        ),
        test_cml_use_config_bad_vector(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "vector",
                "element": { "type": "bool"},
                // Missing max count.
            },
        ],}),
        Err(Error::Validate {err,  .. })
        if &err == "Config 'fuchsia.config.MyConfig' is type Vector but is missing field 'max_count'"
        ),
        test_cml_use_config_bad_string(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "string",
                // Missing max size.
            },
        ],}),
        Err(Error::Validate { err, .. })
        if &err == "Config 'fuchsia.config.MyConfig' is type String but is missing field 'max_size'"
        ),

        test_cml_optional_use_no_config(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "availability": "optional",
            },
        ],}),
        Err(Error::Validate {err, ..})
        if &err == "Optionally using a config capability without a default requires a matching 'config' section."
        ),
        test_cml_transitional_use_no_config(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "availability": "transitional",
            },
        ],}),
        Err(Error::Validate {err, ..})
        if &err == "Optionally using a config capability without a default requires a matching 'config' section."
        ),
        test_cml_optional_use_bad_type(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "availability": "optional",
            },
        ],
        "config": {
            "my_config": { "type": "uint8"}
        }}),
        Err(Error::Validate {err, ..})
        if &err == "Use and config block differ on type for key 'my_config'"
        ),

        test_config_required_with_default(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "default": "true",
            },
        ]}),
        Err(Error::Validate {err, ..})
        if &err == "Config 'fuchsia.config.MyConfig' is required and has a default value"
        ),

        test_cml_optional_use_good(
        json!({"use": [
            {
                "config": "fuchsia.config.MyConfig",
                "key": "my_config",
                "type": "bool",
                "availability": "optional",
            },
        ],
        "config": {
            "my_config": { "type": "bool"},
        }
    }),
    Ok(())
        ),
    test_cml_use_two_types_bad(
        json!({"use": [
            {
                "protocol": "fuchsia.protocol.MyProtocol",
                "service": "fuchsia.service.MyService",
            },
        ],
    }),
        Err(Error::Validate {err, ..})
        if &err == "use declaration has multiple capability types defined: [\"service\", \"protocol\"]"
        ),
    test_cml_offer_two_types_bad(
        json!({"offer": [
            {
                "protocol": "fuchsia.protocol.MyProtocol",
                "service": "fuchsia.service.MyService",
                "from": "self",
                "to" : "#child",
            },
        ],
    }),
        Err(Error::Validate {err, ..})
        if &err == "offer declaration has multiple capability types defined: [\"service\", \"protocol\"]"
        ),
    test_cml_expose_two_types_bad(
        json!({"expose": [
            {
                "protocol": "fuchsia.protocol.MyProtocol",
                "service": "fuchsia.service.MyService",
                "from" : "self",
            },
        ],
    }),
        Err(Error::Validate {err, ..})
        if &err == "expose declaration has multiple capability types defined: [\"service\", \"protocol\"]"
        ),
    }
}
