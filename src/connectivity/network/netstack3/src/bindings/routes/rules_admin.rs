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
    FidlRuleAdminIpExt, InstalledRule, MarkSelector, RuleAction, RuleIndex, RuleSelector,
    RuleSetPriority, RuleSetRequest, RuleTableRequest,
};
use fnet_routes_ext::Responder;
use futures::channel::{mpsc, oneshot};
use futures::TryStreamExt as _;
use net_types::ip::{Ip, Subnet};
use {
    fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext, fuchsia_zircon as zx,
};

use crate::bindings::util::TaskWaitGroupSpawner;
use crate::bindings::{routes, Ctx};

#[derive(Debug, Clone)]
pub(super) struct AddableSelector<I: Ip> {
    /// Matches whether the source address of the packet is from the subnet.
    from: Option<Subnet<I::Addr>>,
    /// Matches the packet iff the packet was locally generated.
    locally_generated: Option<bool>,
    /// Matches the packet iff the socket that was bound to the device using
    /// `SO_BINDTODEVICE`.
    bound_device: Option<fnet_routes_ext::rules::InterfaceSelector>,
    /// The selector for the MARK_1 domain.
    mark_1_selector: Option<MarkSelector>,
    /// The selector for the MARK_2 domain.
    mark_2_selector: Option<MarkSelector>,
}

impl<I: Ip> From<RuleSelector<I>> for AddableSelector<I> {
    fn from(selector: RuleSelector<I>) -> Self {
        let RuleSelector {
            from,
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
        } = selector;
        Self { from, locally_generated, bound_device, mark_1_selector, mark_2_selector }
    }
}

impl<I: Ip> From<AddableSelector<I>> for RuleSelector<I> {
    fn from(selector: AddableSelector<I>) -> Self {
        let AddableSelector {
            from,
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
        } = selector;
        Self { from, locally_generated, bound_device, mark_1_selector, mark_2_selector }
    }
}

pub(super) enum RuleOp<I: Ip> {
    Add {
        priority: RuleSetPriority,
        index: RuleIndex,
        selector: AddableSelector<I>,
        action: RuleAction,
    },
    Remove {
        priority: RuleSetPriority,
        index: RuleIndex,
    },
    RemoveSet {
        priority: RuleSetPriority,
    },
    AuthenticateForRouteTable {
        table_id: routes::TableId<I>,
        token: zx::Event,
    },
}

pub(super) struct NewRuleSet<I: Ip> {
    pub(super) priority: RuleSetPriority,
    pub(super) rule_set_work_receiver: mpsc::UnboundedReceiver<RuleWorkItem<I>>,
    pub(super) responder: oneshot::Sender<Result<(), SetPriorityConflict>>,
}

pub(super) struct RuleWorkItem<I: Ip> {
    pub(super) op: RuleOp<I>,
    pub(super) responder: oneshot::Sender<Result<(), fnet_routes_admin::RuleSetError>>,
}

#[derive(Debug)]
struct Rule<I: Ip> {
    selector: AddableSelector<I>,
    action: RuleAction,
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
            rule_set.rules.into_iter().map(move |(index, Rule { selector, action })| {
                let selector = selector.into();
                InstalledRule { priority, index, selector, action }
            })
        })
    }

    pub(super) fn add_rule(
        &mut self,
        priority: RuleSetPriority,
        index: RuleIndex,
        selector: AddableSelector<I>,
        action: RuleAction,
    ) -> Result<(), fnet_routes_admin::RuleSetError> {
        let mut set = self.get_rule_set_entry(priority);
        match set.get_mut().rules.entry(index) {
            BTreeEntry::Vacant(entry) => {
                let _: &mut Rule<I> = entry.insert(Rule { selector, action });
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
                let Rule { selector, action } = entry.remove();
                let selector = selector.into();
                Ok(InstalledRule { priority, index, selector, action })
            }
            BTreeEntry::Vacant(_entry) => Err(fnet_routes_admin::RuleSetError::RuleDoesNotExist),
        }
    }
}

struct UserRuleSet<I: Ip> {
    priority: RuleSetPriority,
    rule_work_sink: mpsc::UnboundedSender<RuleWorkItem<I>>,
    route_table_authorization_set: HashSet<routes::TableId<I>>,
}

#[derive(Debug)]
enum ApplyRuleOpError {
    RuleSetClosed,
    RuleSetError(fnet_routes_admin::RuleSetError),
}

impl From<fnet_routes_admin::RuleSetError> for ApplyRuleOpError {
    fn from(err: fnet_routes_admin::RuleSetError) -> Self {
        Self::RuleSetError(err)
    }
}

impl ApplyRuleOpError {
    fn respond_result_with<
        R: Responder<ControlHandle: Clone, Payload = Result<(), fnet_routes_admin::RuleSetError>>,
    >(
        result: Result<(), Self>,
        responder: R,
    ) -> Result<ControlFlow<R::ControlHandle>, fidl::Error> {
        match result {
            Err(ApplyRuleOpError::RuleSetClosed) => {
                Ok(ControlFlow::Break(responder.control_handle().clone()))
            }
            Err(ApplyRuleOpError::RuleSetError(err)) => {
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
        selector: RuleSelector<I>,
        action: RuleAction,
    ) -> Result<(), ApplyRuleOpError> {
        let selector = AddableSelector::from(selector);
        if let RuleAction::Lookup(table_id) = action {
            let table_id = routes::TableId::new(table_id)
                .ok_or(fnet_routes_admin::RuleSetError::Unauthenticated)?;
            if !self.route_table_authorization_set.contains(&table_id) {
                Err(fnet_routes_admin::RuleSetError::Unauthenticated)?;
            }
        }
        self.apply_rule_op(RuleOp::Add { priority, index, selector, action }).await
    }

    async fn authenticate_for_route_table(
        &mut self,
        table: u32,
        token: zx::Event,
    ) -> Result<(), ApplyRuleOpError> {
        let table_id = routes::TableId::new(table)
            .ok_or(fnet_routes_admin::RuleSetError::BadAuthentication)?;
        self.apply_rule_op(RuleOp::AuthenticateForRouteTable { table_id, token }).await?;
        let _: bool = self.route_table_authorization_set.insert(table_id);
        Ok(())
    }

    async fn handle_request(
        &mut self,
        request: RuleSetRequest<I>,
    ) -> Result<ControlFlow<I::RuleSetControlHandle>, fidl::Error> {
        match request {
            RuleSetRequest::AddRule { index, selector, action, responder } => {
                let selector = match selector {
                    Ok(selector) => selector,
                    Err(err) => {
                        log::warn!("error addding a rule: {err:?}");
                        return responder
                            .send(Err(fnet_routes_admin::RuleSetError::BaseSelectorMissing))
                            .map(ControlFlow::Continue);
                    }
                };
                let result = self.add_fidl_rule(self.priority, index, selector, action).await;
                ApplyRuleOpError::respond_result_with(result, responder)
            }
            RuleSetRequest::RemoveRule { index, responder } => {
                let priority = self.priority;

                let result = self.apply_rule_op(RuleOp::Remove { priority, index }).await;
                ApplyRuleOpError::respond_result_with(result, responder)
            }
            RuleSetRequest::AuthenticateForRouteTable { table, token, responder } => {
                let result = self.authenticate_for_route_table(table, token).await;
                ApplyRuleOpError::respond_result_with(result, responder)
            }
            RuleSetRequest::Close { control_handle } => Ok(ControlFlow::Break(control_handle)),
        }
    }

    async fn apply_rule_op(&self, op: RuleOp<I>) -> Result<(), ApplyRuleOpError> {
        let (responder, receiver) = oneshot::channel();
        self.rule_work_sink
            .unbounded_send(RuleWorkItem { op, responder })
            .map_err(|mpsc::TrySendError { .. }| ApplyRuleOpError::RuleSetClosed)?;
        receiver
            .await
            .expect("responder should not be dropped")
            .map_err(ApplyRuleOpError::RuleSetError)
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
            assert_matches!(err, ApplyRuleOpError::RuleSetClosed);
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
                        let rule_set_request_stream = rule_set.into_stream()?;
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
