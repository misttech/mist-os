// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{btree_map, hash_map, BTreeMap, HashMap};

use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext as fnet_filter_ext;

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    matchers: fnet_filter_ext::Matchers,
    action: fnet_filter_ext::Action,
}

#[derive(Debug, Clone)]
struct Routine {
    routine_type: fnet_filter_ext::RoutineType,
    rules: BTreeMap<u32, Rule>,
}

impl Routine {
    fn removal_events(self, id: fnet_filter_ext::RoutineId) -> impl Iterator<Item = Event> {
        let Self { rules, routine_type: _ } = self;
        let routine_id = id.clone();
        rules
            .into_keys()
            .map(move |index| {
                Event::Removed(fnet_filter_ext::ResourceId::Rule(fnet_filter_ext::RuleId {
                    routine: routine_id.clone(),
                    index,
                }))
            })
            .chain(std::iter::once(Event::Removed(fnet_filter_ext::ResourceId::Routine(id))))
    }
}

#[derive(Debug, Clone)]
struct Namespace {
    domain: fnet_filter_ext::Domain,
    routines: HashMap<String, Routine>,
}

impl Namespace {
    fn removal_events(self, id: fnet_filter_ext::NamespaceId) -> impl Iterator<Item = Event> {
        let Self { routines, domain: _ } = self;
        let namespace_id = id.clone();
        routines
            .into_iter()
            .flat_map(move |(routine_id, routine)| {
                let routine_id = fnet_filter_ext::RoutineId {
                    namespace: namespace_id.clone(),
                    name: routine_id,
                };
                routine.removal_events(routine_id)
            })
            .chain(std::iter::once(Event::Removed(fnet_filter_ext::ResourceId::Namespace(id))))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Controller {
    namespaces: HashMap<fnet_filter_ext::NamespaceId, Namespace>,
}

#[derive(Clone)]
pub(crate) enum Event {
    Added(fnet_filter_ext::Resource),
    Removed(fnet_filter_ext::ResourceId),
}

impl Controller {
    pub(crate) fn existing_ids(&self) -> impl Iterator<Item = fnet_filter_ext::ResourceId> + '_ {
        self.namespaces.iter().flat_map(|(namespace_id, Namespace { domain: _, routines })| {
            let namespace = fnet_filter_ext::ResourceId::Namespace(namespace_id.clone());
            let routines =
                routines.iter().flat_map(|(routine_id, Routine { routine_type: _, rules })| {
                    let routine =
                        fnet_filter_ext::ResourceId::Routine(fnet_filter_ext::RoutineId {
                            namespace: namespace_id.clone(),
                            name: routine_id.clone(),
                        });
                    let rules = rules.keys().map(|index| {
                        fnet_filter_ext::ResourceId::Rule(fnet_filter_ext::RuleId {
                            routine: fnet_filter_ext::RoutineId {
                                namespace: namespace_id.clone(),
                                name: routine_id.clone(),
                            },
                            index: *index,
                        })
                    });
                    std::iter::once(routine).chain(rules)
                });
            std::iter::once(namespace).chain(routines)
        })
    }

    pub(crate) fn existing_resources(
        &self,
    ) -> impl Iterator<Item = fnet_filter_ext::Resource> + '_ {
        self.namespaces.iter().flat_map(|(namespace_id, Namespace { domain, routines })| {
            let namespace = fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace {
                id: namespace_id.clone(),
                domain: domain.clone(),
            });
            let routines =
                routines.iter().flat_map(|(routine_id, Routine { routine_type, rules })| {
                    let routine = fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine {
                        id: fnet_filter_ext::RoutineId {
                            namespace: namespace_id.clone(),
                            name: routine_id.clone(),
                        },
                        routine_type: routine_type.clone(),
                    });
                    let rules = rules.iter().map(|(index, Rule { matchers, action })| {
                        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule {
                            id: fnet_filter_ext::RuleId {
                                routine: fnet_filter_ext::RoutineId {
                                    namespace: namespace_id.clone(),
                                    name: routine_id.clone(),
                                },
                                index: *index,
                            },
                            matchers: matchers.clone(),
                            action: action.clone(),
                        })
                    });
                    std::iter::once(routine).chain(rules)
                });
            std::iter::once(namespace).chain(routines)
        })
    }

    pub(crate) fn validate_and_apply(
        &mut self,
        changes: Vec<fnet_filter_ext::Change>,
        idempotent: bool,
    ) -> Result<Vec<Event>, (usize, fnet_filter::CommitError)> {
        let validator = Validator::new(self.namespaces.clone());
        let (result, events) = validator.validate(changes, idempotent)?;

        // Only if validation was successful do we actually commit the changes.
        // This ensures that the state will never be only partially updated.
        self.namespaces = result;

        Ok(events)
    }
}

struct Validator {
    namespaces: HashMap<fnet_filter_ext::NamespaceId, Namespace>,
}

impl Validator {
    fn new(namespaces: HashMap<fnet_filter_ext::NamespaceId, Namespace>) -> Self {
        Self { namespaces }
    }

    fn validate(
        mut self,
        changes: Vec<fnet_filter_ext::Change>,
        idempotent: bool,
    ) -> Result<
        (HashMap<fnet_filter_ext::NamespaceId, Namespace>, Vec<Event>),
        (usize, fnet_filter::CommitError),
    > {
        let mut events = Vec::new();
        for (i, change) in changes.into_iter().enumerate() {
            match change {
                fnet_filter_ext::Change::Create(resource) => {
                    self.add_resource(resource, idempotent).map(|event| {
                        if let Some(event) = event {
                            events.push(event);
                        }
                    })
                }
                fnet_filter_ext::Change::Remove(id) => {
                    self.remove_resource(id, idempotent).map(|removals| events.extend(removals))
                }
            }
            .map_err(|e| (i, e))?;
        }
        let Self { namespaces } = self;
        Ok((namespaces, events))
    }

    fn add_resource(
        &mut self,
        resource: fnet_filter_ext::Resource,
        idempotent: bool,
    ) -> Result<Option<Event>, fnet_filter::CommitError> {
        match resource.clone() {
            fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace { id, domain }) => {
                match self.namespaces.entry(id) {
                    hash_map::Entry::Vacant(entry) => {
                        let _ = entry.insert(Namespace { domain, routines: HashMap::new() });
                    }
                    hash_map::Entry::Occupied(entry) => {
                        // Note that if `idempotent` is set, we allow a namespace creation to
                        // succeed when that namespace already exists, as long as it has the same
                        // `domain` as the one being added. This is true even if the existing
                        // namespace already has routines configured.
                        if idempotent && entry.get().domain == domain {
                            return Ok(None);
                        } else {
                            return Err(fnet_filter::CommitError::AlreadyExists);
                        }
                    }
                }
            }
            fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine { id, routine_type }) => {
                let fnet_filter_ext::RoutineId { namespace, name } = id;
                let namespace = self
                    .namespaces
                    .get_mut(&namespace)
                    .ok_or(fnet_filter::CommitError::NamespaceNotFound)?;
                match namespace.routines.entry(name) {
                    hash_map::Entry::Vacant(entry) => {
                        let _ = entry.insert(Routine { routine_type, rules: BTreeMap::default() });
                    }
                    hash_map::Entry::Occupied(entry) => {
                        // Note that if `idempotent` is set, we allow a routine creation to succeed
                        // when that routine already exists, as long as it has the same
                        // `routine_type` as the one being added. This is true even if the existing
                        // routine already has rules added to it.
                        if idempotent && entry.get().routine_type == routine_type {
                            return Ok(None);
                        } else {
                            return Err(fnet_filter::CommitError::AlreadyExists);
                        }
                    }
                }
            }
            fnet_filter_ext::Resource::Rule(rule) => {
                let fnet_filter_ext::Rule { id, matchers, action } = rule;
                let fnet_filter_ext::RuleId { routine, index } = id;
                let fnet_filter_ext::RoutineId { namespace, name: routine } = routine;

                let namespace = self
                    .namespaces
                    .get_mut(&namespace)
                    .ok_or(fnet_filter::CommitError::NamespaceNotFound)?;

                match &action {
                    fnet_filter_ext::Action::Jump(target) => {
                        if !namespace.routines.contains_key(target) {
                            return Err(fnet_filter::CommitError::RoutineNotFound);
                        }
                    }
                    fnet_filter_ext::Action::Accept
                    | fnet_filter_ext::Action::Drop
                    | fnet_filter_ext::Action::Return => {}
                }

                let to_insert = Rule { matchers, action };
                match namespace
                    .routines
                    .get_mut(&routine)
                    .ok_or(fnet_filter::CommitError::RoutineNotFound)?
                    .rules
                    .entry(index)
                {
                    btree_map::Entry::Vacant(entry) => {
                        let _ = entry.insert(to_insert);
                    }
                    btree_map::Entry::Occupied(entry) => {
                        // Note that if `idempotent` is set, we allow a rule creation to succeed
                        // when that routine already exists, as long as it has the same properties
                        // (matcher and action) as the one being added.
                        if idempotent && entry.get() == &to_insert {
                            return Ok(None);
                        } else {
                            return Err(fnet_filter::CommitError::AlreadyExists);
                        }
                    }
                }
            }
        }

        Ok(Some(Event::Added(resource)))
    }

    fn remove_resource(
        &mut self,
        id: fnet_filter_ext::ResourceId,
        idempotent: bool,
    ) -> Result<Vec<Event>, fnet_filter::CommitError> {
        let not_found_result = |err| if !idempotent { Err(err) } else { Ok(Vec::new()) };

        match id {
            fnet_filter_ext::ResourceId::Namespace(id) => match self.namespaces.entry(id.clone()) {
                hash_map::Entry::Vacant(_) => {
                    not_found_result(fnet_filter::CommitError::NamespaceNotFound)
                }
                hash_map::Entry::Occupied(entry) => {
                    let namespace = entry.remove();
                    Ok(namespace.removal_events(id).collect())
                }
            },
            fnet_filter_ext::ResourceId::Routine(id) => {
                let fnet_filter_ext::RoutineId { namespace, name } = &id;
                let Some(namespace) = self.namespaces.get_mut(namespace) else {
                    return not_found_result(fnet_filter::CommitError::NamespaceNotFound);
                };
                match namespace.routines.entry(name.clone()) {
                    hash_map::Entry::Vacant(_) => {
                        not_found_result(fnet_filter::CommitError::RoutineNotFound)
                    }
                    hash_map::Entry::Occupied(entry) => {
                        let routine = entry.remove();
                        Ok(routine.removal_events(id).collect())
                    }
                }
            }
            fnet_filter_ext::ResourceId::Rule(id) => {
                let fnet_filter_ext::RuleId { routine, index } = &id;
                let fnet_filter_ext::RoutineId { namespace, name: routine } = routine;
                let Some(namespace) = self.namespaces.get_mut(namespace) else {
                    return not_found_result(fnet_filter::CommitError::NamespaceNotFound);
                };
                let Some(routine) = namespace.routines.get_mut(routine) else {
                    return not_found_result(fnet_filter::CommitError::RoutineNotFound);
                };
                match routine.rules.entry(*index) {
                    btree_map::Entry::Vacant(_) => {
                        not_found_result(fnet_filter::CommitError::RuleNotFound)
                    }
                    btree_map::Entry::Occupied(entry) => {
                        let _ = entry.remove();
                        Ok(vec![Event::Removed(fnet_filter_ext::ResourceId::Rule(id))])
                    }
                }
            }
        }
    }
}
