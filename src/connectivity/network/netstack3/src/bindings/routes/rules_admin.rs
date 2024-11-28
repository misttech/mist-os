// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::btree_map::{Entry as BTreeEntry, OccupiedEntry as BTreeOccupiedEntry};
use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;
use std::pin::pin;

use assert_matches::assert_matches;
use fidl::endpoints::{ControlHandle as _, ProtocolMarker as _};
use fnet_routes_ext::rules::{
    FidlRuleAdminIpExt, InstalledRule, MarkMatcher, RuleAction, RuleIndex, RuleMatcher,
    RuleSetPriority, RuleSetRequest, RuleTableRequest,
};
use fnet_routes_ext::Responder;
use futures::channel::{mpsc, oneshot};
use futures::TryStreamExt as _;
use net_types::ip::Ip;
use {
    fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext,
};

use crate::bindings::util::{TaskWaitGroupSpawner, TryFromFidl, TryIntoCore as _};
use crate::bindings::{routes, Ctx};
pub(super) use witness::AddableMatcher;

impl<I: Ip> TryFromFidl<RuleMatcher<I>> for netstack3_core::routes::RuleMatcher<I> {
    type Error = fnet_routes_admin::RuleSetError;

    fn try_from_fidl(matcher: RuleMatcher<I>) -> Result<Self, Self::Error> {
        let RuleMatcher { from, locally_generated, bound_device, mark_1, mark_2 } = matcher;
        let traffic_origin_matcher = match (locally_generated, bound_device) {
            (None, None) => None,
            (None, Some(fnet_routes_ext::rules::InterfaceMatcher::DeviceName(name))) => {
                Some(netstack3_core::routes::TrafficOriginMatcher::Local {
                    bound_device_matcher: Some(netstack3_core::device::DeviceNameMatcher(name)),
                })
            }
            (Some(true), None) => Some(netstack3_core::routes::TrafficOriginMatcher::Local {
                bound_device_matcher: None,
            }),
            (Some(false), None) => Some(netstack3_core::routes::TrafficOriginMatcher::NonLocal),
            (Some(true), Some(fnet_routes_ext::rules::InterfaceMatcher::DeviceName(name))) => {
                Some(netstack3_core::routes::TrafficOriginMatcher::Local {
                    bound_device_matcher: Some(netstack3_core::device::DeviceNameMatcher(name)),
                })
            }
            (Some(false), Some(_)) => return Err(fnet_routes_admin::RuleSetError::InvalidMatcher),
        };

        fn to_core_mark_matcher(matcher: MarkMatcher) -> netstack3_core::routes::MarkMatcher {
            match matcher {
                MarkMatcher::Unmarked => netstack3_core::routes::MarkMatcher::Unmarked,
                MarkMatcher::Marked { mask, between } => {
                    netstack3_core::routes::MarkMatcher::Marked {
                        mask,
                        start: *between.start(),
                        end: *between.end(),
                    }
                }
            }
        }

        Ok(netstack3_core::routes::RuleMatcher {
            source_address_matcher: from.map(netstack3_core::ip::SubnetMatcher),
            traffic_origin_matcher,
            mark_matchers: netstack3_core::routes::MarkMatchers::new(
                mark_1
                    .into_iter()
                    .map(|m| (netstack3_core::routes::MarkDomain::Mark1, to_core_mark_matcher(m)))
                    .chain(mark_2.into_iter().map(|m| {
                        (netstack3_core::routes::MarkDomain::Mark2, to_core_mark_matcher(m))
                    })),
            ),
        })
    }
}

pub(super) enum RuleOp<I: Ip> {
    Add {
        priority: RuleSetPriority,
        index: RuleIndex,
        matcher: AddableMatcher<I>,
        action: RuleAction,
    },
    Remove {
        priority: RuleSetPriority,
        index: RuleIndex,
    },
    RemoveSet {
        priority: RuleSetPriority,
    },
}

pub(super) struct NewRuleSet<I: Ip> {
    pub(super) priority: RuleSetPriority,
    pub(super) rule_set_work_receiver: mpsc::UnboundedReceiver<RuleWorkItem<I>>,
    pub(super) responder: oneshot::Sender<Result<(), SetPriorityConflict>>,
}

pub(super) enum RuleWorkItem<I: Ip> {
    RuleOp {
        op: RuleOp<I>,
        responder: oneshot::Sender<Result<(), fnet_routes_admin::RuleSetError>>,
    },
    AuthenticateForRouteTable {
        table_id: routes::TableId<I>,
        token: zx::Event,
        responder: oneshot::Sender<Result<(), fnet_routes_admin::AuthenticateForRouteTableError>>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct Rule<I: Ip> {
    pub(super) matcher: AddableMatcher<I>,
    pub(super) action: RuleAction,
}

#[derive(Debug, Default)]
struct RuleSet<I: Ip> {
    rules: BTreeMap<RuleIndex, Rule<I>>,
}

#[derive(Debug)]
pub(super) struct SetPriorityConflict;

#[derive(Debug, Default)]
pub(super) struct RuleTable<I: Ip> {
    rule_sets: BTreeMap<RuleSetPriority, RuleSet<I>>,
}

impl<I: Ip> RuleTable<I> {
    pub(super) fn new_rule_set(
        &mut self,
        priority: RuleSetPriority,
    ) -> Result<(), SetPriorityConflict> {
        match self.rule_sets.entry(priority) {
            BTreeEntry::Vacant(entry) => {
                let _: &mut RuleSet<I> = entry.insert(RuleSet::default());
                Ok(())
            }
            BTreeEntry::Occupied(_entry) => Err(SetPriorityConflict),
        }
    }

    fn get_rule_set_entry(
        &mut self,
        priority: RuleSetPriority,
    ) -> BTreeOccupiedEntry<'_, RuleSetPriority, RuleSet<I>> {
        match self.rule_sets.entry(priority) {
            BTreeEntry::Occupied(entry) => entry,
            BTreeEntry::Vacant(_vacant) => {
                panic!("the rule set at {priority:?} must exist")
            }
        }
    }

    pub(super) fn remove_rule_set<'c>(
        &mut self,
        priority: RuleSetPriority,
    ) -> impl Iterator<Item = InstalledRule<I>> + 'c {
        let removed = self.rule_sets.remove(&priority);
        removed.into_iter().flat_map(move |rule_set| {
            rule_set.rules.into_iter().map(move |(index, Rule { matcher, action })| {
                let matcher = matcher.into();
                InstalledRule { priority, index, matcher, action }
            })
        })
    }

    pub(super) fn add_rule(
        &mut self,
        priority: RuleSetPriority,
        index: RuleIndex,
        matcher: AddableMatcher<I>,
        action: RuleAction,
    ) -> Result<(), fnet_routes_admin::RuleSetError> {
        let mut set = self.get_rule_set_entry(priority);
        match set.get_mut().rules.entry(index) {
            BTreeEntry::Vacant(entry) => {
                let _: &mut Rule<I> = entry.insert(Rule { matcher, action });
                Ok(())
            }
            BTreeEntry::Occupied(_entry) => Err(fnet_routes_admin::RuleSetError::RuleAlreadyExists),
        }
    }

    pub(super) fn remove_rule(
        &mut self,
        priority: RuleSetPriority,
        index: RuleIndex,
    ) -> Result<InstalledRule<I>, fnet_routes_admin::RuleSetError> {
        let mut set = self.get_rule_set_entry(priority);
        match set.get_mut().rules.entry(index) {
            BTreeEntry::Occupied(entry) => {
                let Rule { matcher, action } = entry.remove();
                let matcher = matcher.into();
                Ok(InstalledRule { priority, index, matcher, action })
            }
            BTreeEntry::Vacant(_entry) => Err(fnet_routes_admin::RuleSetError::RuleDoesNotExist),
        }
    }

    pub(super) fn handle_table_removed(
        &mut self,
        removed_table_id: routes::TableId<I>,
    ) -> Vec<InstalledRule<I>> {
        // TODO(https://github.com/rust-lang/rust/issues/70530): Use `extract_if`.
        let mut removed = Vec::new();
        for (priority, set) in self.rule_sets.iter_mut() {
            set.rules.retain(|index, Rule { matcher, action }| {
                let table_id = match action {
                    RuleAction::Unreachable => None,
                    RuleAction::Lookup(id) => Some(*id),
                };

                if table_id.is_some_and(|id| id == u32::from(removed_table_id)) {
                    removed.push(InstalledRule {
                        priority: *priority,
                        index: *index,
                        matcher: matcher.clone().into(),
                        action: *action,
                    });
                    false
                } else {
                    true
                }
            })
        }
        removed
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &'_ Rule<I>> + '_ {
        self.rule_sets.values().flat_map(|set| set.rules.values())
    }
}

struct UserRuleSet<I: Ip> {
    priority: RuleSetPriority,
    rule_work_sink: mpsc::UnboundedSender<RuleWorkItem<I>>,
    route_table_authorization_set: HashSet<routes::TableId<I>>,
}

#[derive(Debug)]
enum ApplyRuleWorkError<E> {
    RuleSetClosed,
    RuleWorkError(E),
}

impl<E> From<E> for ApplyRuleWorkError<E> {
    fn from(err: E) -> Self {
        Self::RuleWorkError(err)
    }
}

impl<E> ApplyRuleWorkError<E> {
    fn respond_result_with<R: Responder<ControlHandle: Clone, Payload = Result<(), E>>>(
        result: Result<(), Self>,
        responder: R,
    ) -> Result<ControlFlow<R::ControlHandle>, fidl::Error> {
        match result {
            Err(ApplyRuleWorkError::RuleSetClosed) => {
                Ok(ControlFlow::Break(responder.control_handle().clone()))
            }
            Err(ApplyRuleWorkError::RuleWorkError(err)) => {
                responder.send(Err(err)).map(ControlFlow::Continue)
            }
            Ok(()) => responder.send(Ok(())).map(ControlFlow::Continue),
        }
    }
}

impl<I: Ip + FidlRuleAdminIpExt> UserRuleSet<I> {
    async fn add_fidl_rule(
        &self,
        priority: RuleSetPriority,
        index: RuleIndex,
        matcher: RuleMatcher<I>,
        action: RuleAction,
    ) -> Result<(), ApplyRuleWorkError<fnet_routes_admin::RuleSetError>> {
        let matcher = AddableMatcher::try_from(matcher)?;
        if let RuleAction::Lookup(table_id) = action {
            let table_id = routes::TableId::new(table_id)
                .ok_or(fnet_routes_admin::RuleSetError::Unauthenticated)?;
            if !self.route_table_authorization_set.contains(&table_id) {
                Err(fnet_routes_admin::RuleSetError::Unauthenticated)?;
            }
        }
        self.apply_rule_op(RuleOp::Add { priority, index, matcher, action }).await
    }

    async fn authenticate_for_route_table(
        &mut self,
        table: u32,
        token: zx::Event,
    ) -> Result<(), ApplyRuleWorkError<fnet_routes_admin::AuthenticateForRouteTableError>> {
        let table_id = routes::TableId::new(table)
            .ok_or(fnet_routes_admin::AuthenticateForRouteTableError::InvalidAuthentication)?;
        let (responder, receiver) = oneshot::channel();
        self.rule_work_sink
            .unbounded_send(RuleWorkItem::AuthenticateForRouteTable { table_id, token, responder })
            .map_err(|mpsc::TrySendError { .. }| ApplyRuleWorkError::RuleSetClosed)?;
        receiver
            .await
            .expect("responder should not be dropped")
            .map_err(ApplyRuleWorkError::RuleWorkError)?;
        let _: bool = self.route_table_authorization_set.insert(table_id);
        Ok(())
    }

    async fn handle_request(
        &mut self,
        request: RuleSetRequest<I>,
    ) -> Result<ControlFlow<I::RuleSetControlHandle>, fidl::Error> {
        match request {
            RuleSetRequest::AddRule { index, matcher, action, responder } => {
                let matcher = match matcher {
                    Ok(matcher) => matcher,
                    Err(err) => {
                        log::warn!("error addding a rule: {err:?}");
                        return responder
                            .send(Err(fnet_routes_admin::RuleSetError::BaseMatcherMissing))
                            .map(ControlFlow::Continue);
                    }
                };
                let result = self.add_fidl_rule(self.priority, index, matcher, action).await;
                ApplyRuleWorkError::respond_result_with(result, responder)
            }
            RuleSetRequest::RemoveRule { index, responder } => {
                let priority = self.priority;

                let result = self.apply_rule_op(RuleOp::Remove { priority, index }).await;
                ApplyRuleWorkError::respond_result_with(result, responder)
            }
            RuleSetRequest::AuthenticateForRouteTable { table, token, responder } => {
                let result = self.authenticate_for_route_table(table, token).await;
                ApplyRuleWorkError::respond_result_with(result, responder)
            }
            RuleSetRequest::Close { control_handle } => Ok(ControlFlow::Break(control_handle)),
        }
    }

    async fn apply_rule_op(
        &self,
        op: RuleOp<I>,
    ) -> Result<(), ApplyRuleWorkError<fnet_routes_admin::RuleSetError>> {
        let (responder, receiver) = oneshot::channel();
        self.rule_work_sink
            .unbounded_send(RuleWorkItem::RuleOp { op, responder })
            .map_err(|mpsc::TrySendError { .. }| ApplyRuleWorkError::RuleSetClosed)?;
        receiver
            .await
            .expect("responder should not be dropped")
            .map_err(ApplyRuleWorkError::RuleWorkError)
    }
}

async fn serve_rule_set<I: FidlRuleAdminIpExt>(
    stream: I::RuleSetRequestStream,
    mut set: UserRuleSet<I>,
) {
    let mut stream = pin!(stream);

    let control_handle = loop {
        match stream.try_next().await {
            Err(err) => {
                if !err.is_closed() {
                    log::error!("error serving {}: {err:?}", I::RuleSetMarker::DEBUG_NAME);
                    break None;
                }
            }
            Ok(None) => break None,
            Ok(Some(request)) => {
                match set.handle_request(I::into_rule_set_request(request)).await {
                    Ok(ControlFlow::Continue(())) => {}
                    Ok(ControlFlow::Break(control_handle)) => break Some(control_handle),
                    Err(err) => {
                        let level =
                            if err.is_closed() { log::Level::Warn } else { log::Level::Error };
                        log::log!(
                            level,
                            "error serving {}: {:?}",
                            I::RuleSetMarker::DEBUG_NAME,
                            err
                        );
                        break None;
                    }
                }
            }
        }
    };

    match set.apply_rule_op(RuleOp::RemoveSet { priority: set.priority }).await {
        Ok(()) => {}
        Err(err) => {
            assert_matches!(err, ApplyRuleWorkError::RuleSetClosed);
            log::warn!(
                "rule set was already removed when finish serving {}",
                I::RuleSetMarker::DEBUG_NAME
            )
        }
    }

    // This shutdown does nothing because the stream is never polled again, we
    // are actually relying on the drop impl to close the channel. This is just
    // to be explicit that a Close was called.
    if let Some(control_handle) = control_handle {
        control_handle.shutdown();
    }
}

pub(crate) async fn serve_rule_table<I: FidlRuleAdminIpExt>(
    stream: I::RuleTableRequestStream,
    spawner: TaskWaitGroupSpawner,
    ctx: &Ctx,
) -> Result<(), fidl::Error> {
    let mut stream = pin!(stream);

    while let Some(request) = stream.try_next().await? {
        match I::into_rule_table_request(request) {
            RuleTableRequest::NewRuleSet { priority, rule_set, control_handle: _ } => {
                let (rule_work_sink, receiver) = mpsc::unbounded();
                match ctx.bindings_ctx().routes.new_rule_set(priority, receiver).await {
                    Ok(()) => {
                        let rule_set_request_stream = rule_set.into_stream();
                        let rule_set = UserRuleSet {
                            rule_work_sink,
                            priority,
                            route_table_authorization_set: Default::default(),
                        };
                        spawner.spawn(serve_rule_set::<I>(rule_set_request_stream, rule_set));
                    }
                    Err(err) => {
                        log::warn!(
                            "failed to add a new rule set at {priority:?} due to confliction: {err:?}"
                        );
                        rule_set.close_with_epitaph(zx::Status::ALREADY_EXISTS)?;
                    }
                }
            }
        }
    }
    Ok(())
}

mod witness {
    use super::*;

    /// Witness type for validated matchers that can be added to the Core.
    ///
    /// Because FIDL matchers and Core matchers don't match to each other 1:1,
    /// we also store a copy of the FIDL matcher that we can use to respond
    /// to FIDL requests.
    #[derive(Debug, Clone)]
    pub(in crate::bindings::routes) struct AddableMatcher<I: Ip> {
        core: netstack3_core::routes::RuleMatcher<I>,
        fidl: RuleMatcher<I>,
    }

    impl<I: Ip> Default for AddableMatcher<I> {
        fn default() -> Self {
            Self {
                core: netstack3_core::routes::RuleMatcher::match_all_packets(),
                fidl: Default::default(),
            }
        }
    }

    impl<I: Ip> TryFrom<RuleMatcher<I>> for AddableMatcher<I> {
        type Error = fnet_routes_admin::RuleSetError;

        fn try_from(matcher: RuleMatcher<I>) -> Result<Self, Self::Error> {
            let core = matcher.clone().try_into_core()?;
            Ok(Self { core, fidl: matcher })
        }
    }

    impl<I: Ip> From<AddableMatcher<I>> for RuleMatcher<I> {
        fn from(matcher: AddableMatcher<I>) -> Self {
            let AddableMatcher { core: _, fidl } = matcher;
            fidl
        }
    }

    impl<I: Ip> From<AddableMatcher<I>> for netstack3_core::routes::RuleMatcher<I> {
        fn from(matcher: AddableMatcher<I>) -> Self {
            let AddableMatcher { core, fidl: _ } = matcher;
            core
        }
    }
}

#[cfg(test)]
mod tests {
    use net_types::ip::Ipv4;
    use netstack3_core::device::DeviceNameMatcher;
    use netstack3_core::routes::{RuleMatcher as CoreRuleMatcher, TrafficOriginMatcher};
    use test_case::test_case;

    use super::*;

    #[test_case(None, false => Ok(CoreRuleMatcher {
        traffic_origin_matcher: None,
        ..CoreRuleMatcher::match_all_packets()
    }))]
    #[test_case(None, true => Ok(CoreRuleMatcher {
        traffic_origin_matcher: Some(TrafficOriginMatcher::Local {
            bound_device_matcher: Some(DeviceNameMatcher("lo".into())),
        }),
        ..CoreRuleMatcher::match_all_packets()
    }))]
    #[test_case(Some(true), true => Ok(CoreRuleMatcher {
        traffic_origin_matcher: Some(TrafficOriginMatcher::Local {
            bound_device_matcher: Some(DeviceNameMatcher("lo".into())),
        }),
        ..CoreRuleMatcher::match_all_packets()
    }))]
    #[test_case(Some(false), true => Err(fnet_routes_admin::RuleSetError::InvalidMatcher))]
    #[test_case(Some(true), false => Ok(CoreRuleMatcher {
        traffic_origin_matcher: Some(TrafficOriginMatcher::Local {
            bound_device_matcher: None,
        }),
        ..CoreRuleMatcher::match_all_packets()
    }))]
    #[test_case(Some(false), false => Ok(CoreRuleMatcher {
        traffic_origin_matcher: Some(TrafficOriginMatcher::NonLocal),
        ..CoreRuleMatcher::match_all_packets()
    }))]
    fn convert_to_core_matcher(
        locally_generated: Option<bool>,
        has_bound_device_matcher: bool,
    ) -> Result<CoreRuleMatcher<Ipv4>, fnet_routes_admin::RuleSetError> {
        let fidl = RuleMatcher {
            locally_generated,
            bound_device: has_bound_device_matcher
                .then_some(fnet_routes_ext::rules::InterfaceMatcher::DeviceName("lo".into())),
            ..Default::default()
        };
        fidl.try_into_core()
    }
}
