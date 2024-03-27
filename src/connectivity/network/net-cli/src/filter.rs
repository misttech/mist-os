// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net_filter_ext::{
    ControllerId, Domain, NamespaceId, Resource, ResourceId, RoutineId, RoutineType, Rule, RuleId,
    Update,
};
use itertools::Itertools;
use std::collections::{BTreeMap, HashMap};

/// A datatype storing the filtering resource state. Design inspired by nftables.
///
/// nftables: table -> chains -> rules
/// netstack: namespace -> routines -> rules
///
/// Note: The Netstack has an additional datatype, Controller at the top level. Each controller has
/// many namespaces.
#[derive(Debug, Default)]
pub struct FilteringResources {
    controllers: BTreeMap<ControllerId, BTreeMap<NamespaceId, Namespace>>,
}

impl FilteringResources {
    /// Gets an iterator over ControllerId, ordered alphabetically.
    pub fn controllers(&self) -> impl Iterator<Item = &ControllerId> {
        self.controllers.keys()
    }

    /// Gets Namespaces, given a ControllerId.
    pub fn namespaces(
        &self,
        controller_id: &ControllerId,
    ) -> Option<impl Iterator<Item = &Namespace>> {
        let namespaces = self.controllers.get(controller_id)?.values();

        Some(namespaces)
    }

    fn find_namespace_mut(
        &mut self,
        controller_id: &ControllerId,
        namespace_id: &NamespaceId,
    ) -> Option<&mut Namespace> {
        self.controllers.get_mut(controller_id)?.get_mut(namespace_id)
    }

    fn add_namespace(
        &mut self,
        controller_id: ControllerId,
        namespace: fidl_fuchsia_net_filter_ext::Namespace,
    ) -> Option<Resource> {
        let namespace = Namespace::new(namespace);
        let namespaces = self.controllers.entry(controller_id).or_default();

        namespaces
            .insert(namespace.id.clone(), namespace)
            .map(|namespace| Resource::Namespace(namespace.into()))
    }

    fn remove_namespace(
        &mut self,
        controller_id: &ControllerId,
        namespace_id: &NamespaceId,
    ) -> Option<Resource> {
        let namespaces = self.controllers.get_mut(controller_id)?;

        let removed_namespace = namespaces.remove(namespace_id)?;

        // If last namespace deleted, remove the controller.
        if namespaces.is_empty() {
            let _ = self
                .controllers
                .remove(controller_id)
                .expect("Attempted to delete controller but failed.");
        }

        Some(Resource::Namespace(removed_namespace.into()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Namespace {
    pub id: NamespaceId,
    pub domain: Domain,
    routines: HashMap<RoutineId, Routine>,
}

impl Namespace {
    fn new(namespace: fidl_fuchsia_net_filter_ext::Namespace) -> Self {
        Self { id: namespace.id, domain: namespace.domain, routines: HashMap::new() }
    }

    /// Gets an iterator over the Routines of this Namespace.
    pub fn routines(&self) -> impl Iterator<Item = &Routine> {
        self.routines
            .values()
            // TODO(https://fxbug.dev/329686169): Improve data structure and readability of
            // Routines within a Namespace.
            .sorted_by(|routine_a, routine_b| routine_b.priority().cmp(&routine_a.priority()))
    }

    fn find_routine_mut(&mut self, routine_id: &RoutineId) -> Option<&mut Routine> {
        self.routines.get_mut(routine_id)
    }

    fn add_routine(&mut self, routine: fidl_fuchsia_net_filter_ext::Routine) -> Option<Resource> {
        let routine = Routine::new(routine);

        self.routines
            .insert(routine.id.clone(), routine)
            .map(|routine| Resource::Routine(routine.into()))
    }

    fn remove_routine(&mut self, routine_id: &RoutineId) -> Option<Resource> {
        self.routines.remove(routine_id).map(|routine| Resource::Routine(routine.into()))
    }
}

impl Into<fidl_fuchsia_net_filter_ext::Namespace> for Namespace {
    fn into(self) -> fidl_fuchsia_net_filter_ext::Namespace {
        fidl_fuchsia_net_filter_ext::Namespace { id: self.id, domain: self.domain }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    pub id: RoutineId,
    pub routine_type: RoutineType,
    rules: BTreeMap<u32, Rule>,
}

impl Routine {
    fn new(routine: fidl_fuchsia_net_filter_ext::Routine) -> Self {
        Self { id: routine.id, routine_type: routine.routine_type, rules: BTreeMap::new() }
    }

    /// Gets an iterator over the rules in this routine, in order by rule's index.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.values()
    }

    fn priority(&self) -> Option<i32> {
        match &self.routine_type {
            RoutineType::Ip(Some(ip)) => Some(ip.priority),
            RoutineType::Nat(Some(nat)) => Some(nat.priority),
            RoutineType::Ip(None) | RoutineType::Nat(None) => None,
        }
    }

    fn add_rule(&mut self, rule: Rule) -> Option<Resource> {
        self.rules
            .insert(rule.id.index, rule)
            .map(|existing_rule| Resource::Rule(existing_rule.into()))
    }

    fn remove_rule(&mut self, rule_id: &RuleId) -> Option<Resource> {
        self.rules.remove(&rule_id.index).map(|rule| Resource::Rule(rule.into()))
    }
}

impl Into<fidl_fuchsia_net_filter_ext::Routine> for Routine {
    fn into(self) -> fidl_fuchsia_net_filter_ext::Routine {
        fidl_fuchsia_net_filter_ext::Routine { id: self.id, routine_type: self.routine_type }
    }
}

impl Update for FilteringResources {
    fn add(&mut self, controller: ControllerId, resource: Resource) -> Option<Resource> {
        match resource {
            Resource::Namespace(namespace) => self.add_namespace(controller, namespace),
            Resource::Routine(routine) => {
                let namespace = self
                    .find_namespace_mut(&controller, &routine.id.namespace)
                    .expect("Routine's namespace does not exist.");

                namespace.add_routine(routine)
            }
            Resource::Rule(rule) => {
                let namespace = self
                    .find_namespace_mut(&controller, &rule.id.routine.namespace)
                    .expect("Rule's namespace does not exist.");

                let routine = namespace
                    .find_routine_mut(&rule.id.routine)
                    .expect("Rule's routine does not exist.");

                routine.add_rule(rule)
            }
        }
    }

    fn remove(&mut self, controller: ControllerId, resource: &ResourceId) -> Option<Resource> {
        match resource {
            ResourceId::Namespace(namespace_id) => self.remove_namespace(&controller, namespace_id),
            ResourceId::Routine(routine_id) => {
                let namespace = self.find_namespace_mut(&controller, &routine_id.namespace)?;

                namespace.remove_routine(routine_id)
            }
            ResourceId::Rule(rule_id) => {
                let namespace = self.find_namespace_mut(&controller, &rule_id.routine.namespace)?;
                let routine = namespace.find_routine_mut(&rule_id.routine)?;

                routine.remove_rule(rule_id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fidl_fuchsia_net_filter_ext::{Action, InstalledIpRoutine, IpHook, Matchers};

    use super::*;

    /// Test helpers
    impl FilteringResources {
        fn find_namespace(
            &self,
            controller_id: &ControllerId,
            namespace_id: &NamespaceId,
        ) -> Option<&Namespace> {
            self.controllers.get(controller_id)?.get(namespace_id)
        }

        fn namespaces_fidl(
            &self,
            controller_id: ControllerId,
        ) -> Option<Vec<fidl_fuchsia_net_filter_ext::Namespace>> {
            let namespaces = self
                .namespaces(&controller_id)?
                .map(|namespace| namespace.clone().into())
                .collect();

            Some(namespaces)
        }

        fn routines(
            &self,
            controller_id: ControllerId,
            namespace_id: NamespaceId,
        ) -> Option<Vec<fidl_fuchsia_net_filter_ext::Routine>> {
            let routines = self
                .find_namespace(&controller_id, &namespace_id)?
                .routines()
                .map(|routine| routine.clone().into())
                .collect();

            Some(routines)
        }

        fn rules(
            &self,
            controller_id: ControllerId,
            namespace_id: NamespaceId,
            routine_id: RoutineId,
        ) -> Option<Vec<&Rule>> {
            self.find_namespace(&controller_id, &namespace_id)?
                .routines()
                .find(|routine| routine.id == routine_id)
                .map(|routine| routine.rules.values().collect_vec())
        }
    }

    fn test_controller_a() -> ControllerId {
        ControllerId(String::from("test-controller-a"))
    }

    fn test_controller_b() -> ControllerId {
        ControllerId(String::from("test-controller-b"))
    }

    fn test_controller_c() -> ControllerId {
        ControllerId(String::from("test-controller-c"))
    }

    fn test_namespace_a() -> fidl_fuchsia_net_filter_ext::Namespace {
        fidl_fuchsia_net_filter_ext::Namespace {
            id: NamespaceId(String::from("test-namespace-a")),
            domain: Domain::AllIp,
        }
    }

    fn test_namespace_resource_a() -> Resource {
        Resource::Namespace(test_namespace_a())
    }

    fn test_namespace_b() -> fidl_fuchsia_net_filter_ext::Namespace {
        fidl_fuchsia_net_filter_ext::Namespace {
            id: NamespaceId(String::from("test-namespace-b")),
            domain: Domain::AllIp,
        }
    }

    fn test_namespace_resource_b() -> Resource {
        Resource::Namespace(test_namespace_b())
    }

    fn test_routine_a() -> fidl_fuchsia_net_filter_ext::Routine {
        fidl_fuchsia_net_filter_ext::Routine {
            id: RoutineId {
                namespace: test_namespace_a().id,
                name: String::from("test-routine-a"),
            },
            routine_type: RoutineType::Ip(None),
        }
    }

    fn test_routine_with_priority(
        name: String,
        priority: i32,
    ) -> fidl_fuchsia_net_filter_ext::Routine {
        fidl_fuchsia_net_filter_ext::Routine {
            id: RoutineId { namespace: test_namespace_a().id, name },
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                priority,
                hook: IpHook::Ingress,
            })),
        }
    }

    fn test_routine_resource_a() -> Resource {
        Resource::Routine(test_routine_a())
    }

    fn test_routine_b() -> fidl_fuchsia_net_filter_ext::Routine {
        fidl_fuchsia_net_filter_ext::Routine {
            id: RoutineId {
                namespace: test_namespace_b().id,
                name: String::from("test-routine-b"),
            },
            routine_type: RoutineType::Ip(None),
        }
    }

    fn test_routine_resource_b() -> Resource {
        Resource::Routine(test_routine_b())
    }

    fn test_rule_a() -> Rule {
        Rule {
            id: RuleId { routine: test_routine_a().id, index: 1 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }
    }

    fn test_rule_a_with_index(index: u32) -> Rule {
        Rule {
            id: RuleId { routine: test_routine_a().id, index },
            matchers: Matchers::default(),
            action: Action::Accept,
        }
    }

    fn test_rule_resource_a() -> Resource {
        Resource::Rule(test_rule_a())
    }

    #[test]
    fn add_namespace_not_existing() {
        let mut tree = FilteringResources::default();

        let result = tree.add(test_controller_a(), test_namespace_resource_a());

        assert_eq!(result, None);
        assert_eq!(tree.namespaces_fidl(test_controller_a()), Some(vec![test_namespace_a()]));
    }

    #[test]
    fn add_namespace_adds_controller() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        assert_eq!(tree.controllers().collect_vec(), vec![&test_controller_a()]);
    }

    #[test]
    fn controllers_ordered_alphabetically() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_b(), test_namespace_resource_a());
        let _ = tree.add(test_controller_c(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        assert_eq!(
            tree.controllers().collect_vec(),
            vec![&test_controller_a(), &test_controller_b(), &test_controller_c()]
        );
    }

    #[test]
    fn add_namespace_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a()); // already added...

        let result = tree.add(test_controller_a(), test_namespace_resource_a());

        assert_eq!(result, Some(test_namespace_resource_a()));
        assert_eq!(tree.namespaces_fidl(test_controller_a()), Some(vec![test_namespace_a()]));
    }

    #[test]
    fn add_namespace_multiple() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_namespace_resource_b());

        assert_eq!(
            tree.namespaces_fidl(test_controller_a()),
            Some(vec![test_namespace_a(), test_namespace_b(),])
        );
    }

    #[test]
    fn add_namespace_scoped_to_controllers() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_b(), test_namespace_resource_a());

        assert_eq!(tree.namespaces_fidl(test_controller_a()), Some(vec![test_namespace_a()]));
        assert_eq!(tree.namespaces_fidl(test_controller_b()), Some(vec![test_namespace_a()]));
    }

    #[test]
    fn remove_namespace_not_existing() {
        let mut tree = FilteringResources::default();

        let result = tree.remove(test_controller_a(), &test_namespace_resource_a().id());

        assert_eq!(result, None);
        assert_eq!(tree.namespaces_fidl(test_controller_a()), None);
    }

    #[test]
    fn remove_namespace_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        let result = tree.remove(test_controller_a(), &test_namespace_resource_a().id());

        assert_eq!(result, Some(test_namespace_resource_a()));
        assert_eq!(tree.namespaces_fidl(test_controller_a()), None);
    }

    #[test]
    fn remove_namespace_multiple() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_namespace_resource_b());

        let result = tree.remove(test_controller_a(), &test_namespace_resource_a().id());

        assert_eq!(result, Some(test_namespace_resource_a()));
        assert_eq!(tree.namespaces_fidl(test_controller_a()), Some(vec![test_namespace_b(),]));
    }

    #[test]
    fn remove_namespace_scoped_to_controllers() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_b(), test_namespace_resource_a());

        let result = tree.remove(test_controller_a(), &test_namespace_resource_a().id());

        assert_eq!(result, Some(test_namespace_resource_a()));
        assert_eq!(tree.namespaces_fidl(test_controller_a()), None);
        assert_eq!(tree.namespaces_fidl(test_controller_b()), Some(vec![test_namespace_a()]));
    }

    #[test]
    fn remove_last_namespace_on_controller_removes_controller() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_namespace_resource_b());

        let _ = tree.remove(test_controller_a(), &test_namespace_resource_a().id());
        assert_eq!(tree.controllers().collect_vec(), vec![&test_controller_a()]); // still 1 left

        let _ = tree.remove(test_controller_a(), &test_namespace_resource_b().id());
        // now empty
        assert_eq!(tree.controllers().collect_vec(), vec![] as Vec<&ControllerId>);
    }

    #[test]
    #[should_panic(expected = "Routine's namespace does not exist.")]
    fn add_routine_with_namespace_not_existing_panics() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_routine_resource_a());
    }

    #[test]
    fn add_routine_with_namespace_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        let result = tree.add(test_controller_a(), test_routine_resource_a());

        assert_eq!(result, None);
        assert_eq!(
            tree.routines(test_controller_a(), test_namespace_a().id),
            Some(vec![test_routine_a()])
        );
    }

    #[test]
    fn add_routine_with_routine_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_a());

        let result = tree.add(test_controller_a(), test_routine_resource_a());

        assert_eq!(result, Some(test_routine_resource_a()));
        assert_eq!(
            tree.routines(test_controller_a(), test_namespace_a().id),
            Some(vec![test_routine_a()])
        );
    }

    #[test]
    fn add_routine_multiple_different_namespaces() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_namespace_resource_b());

        let _ = tree.add(test_controller_a(), test_routine_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_b());

        assert_eq!(
            tree.routines(test_controller_a(), test_namespace_a().id),
            Some(vec![test_routine_a()])
        );
        assert_eq!(
            tree.routines(test_controller_a(), test_namespace_b().id),
            Some(vec![test_routine_b()])
        );
    }

    #[test]
    fn add_routine_multiple_same_namespaces() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        let routine_a = test_routine_with_priority("test-routine-a".into(), 5);
        let routine_b = test_routine_with_priority("test-routine-b".into(), 4);
        let _ = tree.add(test_controller_a(), Resource::Routine(routine_a.clone()));
        let _ = tree.add(test_controller_a(), Resource::Routine(routine_b.clone()));

        assert_eq!(
            tree.routines(test_controller_a(), test_namespace_a().id),
            Some(vec![routine_a, routine_b])
        );
    }

    #[test]
    fn add_routines_orders_by_priority() {
        let mut namespace = Namespace::new(test_namespace_a());

        let routine_first = test_routine_with_priority("routine-a".to_string(), 14);
        let routine_second = test_routine_with_priority("routine-b".to_string(), 13);
        let routine_third = test_routine_with_priority("routine-c".to_string(), 12);

        let _ = namespace.add_routine(routine_third.clone());
        let _ = namespace.add_routine(routine_first.clone());
        let _ = namespace.add_routine(routine_second.clone());

        let routines: Vec<fidl_fuchsia_net_filter_ext::Routine> =
            namespace.routines().map(|routine| routine.clone().into()).collect();

        assert_eq!(routines, vec![routine_first, routine_second, routine_third]);
    }

    #[test]
    fn remove_routine_not_existing() {
        let mut tree = FilteringResources::default();

        let result = tree.remove(test_controller_a(), &test_routine_resource_a().id());

        assert_eq!(result, None);
        assert_eq!(tree.routines(test_controller_a(), test_namespace_a().id), None);
    }

    #[test]
    fn remove_routine_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_a());

        let result = tree.remove(test_controller_a(), &test_routine_resource_a().id());

        assert_eq!(result, Some(test_routine_resource_a()));
        assert_eq!(tree.routines(test_controller_a(), test_namespace_a().id), Some(vec![]));
    }

    #[test]
    #[should_panic(expected = "Rule's namespace does not exist.")]
    fn add_rule_with_namespace_not_existing_panics() {
        let mut tree = FilteringResources::default();

        let _ = tree.add(test_controller_a(), test_rule_resource_a());
    }

    #[test]
    #[should_panic(expected = "Rule's routine does not exist.")]
    fn add_rule_with_routine_not_existing_panics() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());

        let _ = tree.add(test_controller_a(), test_rule_resource_a());
    }

    #[test]
    fn add_rule_with_namespace_and_routine_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_a());

        let result = tree.add(test_controller_a(), test_rule_resource_a());

        assert_eq!(result, None);
        assert_eq!(
            tree.rules(test_controller_a(), test_namespace_a().id, test_routine_a().id),
            Some(vec![&test_rule_a()])
        );
    }

    #[test]
    fn add_rule_with_routine_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_a());
        let _ = tree.add(test_controller_a(), test_rule_resource_a());

        let result = tree.add(test_controller_a(), test_rule_resource_a());

        assert_eq!(result, Some(test_rule_resource_a()));
        assert_eq!(
            tree.rules(test_controller_a(), test_namespace_a().id, test_routine_a().id),
            Some(vec![&test_rule_a()])
        );
    }

    #[test]
    fn add_rules_orders_by_index() {
        let mut routine = Routine::new(test_routine_a());

        let rule_first = test_rule_a_with_index(12);
        let rule_second = test_rule_a_with_index(13);
        let rule_third = test_rule_a_with_index(14);

        let _ = routine.add_rule(rule_third.clone());
        let _ = routine.add_rule(rule_first.clone());
        let _ = routine.add_rule(rule_second.clone());

        let rules: Vec<&Rule> = routine.rules().collect_vec();

        assert_eq!(rules, vec![&rule_first, &rule_second, &rule_third]);
    }

    #[test]
    fn remove_rule_not_existing() {
        let mut tree = FilteringResources::default();

        let result = tree.remove(test_controller_a(), &test_rule_resource_a().id());

        assert_eq!(result, None);
        assert_eq!(
            tree.rules(test_controller_a(), test_namespace_a().id, test_routine_a().id),
            None
        );
    }

    #[test]
    fn remove_rule_existing() {
        let mut tree = FilteringResources::default();
        let _ = tree.add(test_controller_a(), test_namespace_resource_a());
        let _ = tree.add(test_controller_a(), test_routine_resource_a());
        let _ = tree.add(test_controller_a(), test_rule_resource_a());

        let result = tree.remove(test_controller_a(), &test_rule_resource_a().id());

        assert_eq!(result, Some(test_rule_resource_a()));
        assert_eq!(
            tree.rules(test_controller_a(), test_namespace_a().id, test_routine_a().id),
            Some(vec![])
        );
    }
}
