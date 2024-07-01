// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::btree_map::{Entry as BTreeEntry, OccupiedEntry as BTreeOccupiedEntry};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::pin::pin;

use assert_matches::assert_matches;
use fidl::endpoints::{ControlHandle as _, ProtocolMarker as _};
use fnet_routes_ext::rules::{
    FidlRuleAdminIpExt, InstalledRule, RuleAction, RuleIndex, RuleSelector, RuleSetPriority,
    RuleSetRequest, RuleTableRequest,
};
use fnet_routes_ext::Responder;
use futures::channel::{mpsc, oneshot};
use futures::TryStreamExt as _;
use net_types::ip::Ip;
use {
    fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext, fuchsia_zircon as zx,
};

use crate::bindings::util::TaskWaitGroupSpawner;
use crate::bindings::Ctx;

pub(super) enum RuleOp<I: Ip> {
    Add {
        priority: RuleSetPriority,
        index: RuleIndex,
        selector: RuleSelector<I>,
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

pub(super) struct RuleWorkItem<I: Ip> {
    pub(super) op: RuleOp<I>,
    pub(super) responder: oneshot::Sender<Result<(), fnet_routes_admin::RuleSetError>>,
}

#[derive(Debug)]
struct Rule<I: Ip> {
    selector: RuleSelector<I>,
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

    pub(super) fn remove_rule_set(
        &mut self,
        priority: RuleSetPriority,
    ) -> impl Iterator<Item = InstalledRule<I>> {
        let removed = self.rule_sets.remove(&priority);
        removed.into_iter().flat_map(move |rule_set| {
            rule_set.rules.into_iter().map(move |(index, Rule { selector, action })| {
                InstalledRule { priority, index, selector, action }
            })
        })
    }

    pub(super) fn add_rule(
        &mut self,
        priority: RuleSetPriority,
        index: RuleIndex,
        selector: RuleSelector<I>,
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
                Ok(InstalledRule { priority, index, selector, action })
            }
            BTreeEntry::Vacant(_entry) => Err(fnet_routes_admin::RuleSetError::RuleDoesNotExist),
        }
    }
}
struct UserRuleSet<I: Ip> {
    priority: RuleSetPriority,
    rule_work_sink: mpsc::UnboundedSender<RuleWorkItem<I>>,
}

#[derive(Debug)]
enum ApplyRuleOpError {
    RuleSetClosed,
    RuleSetError(fnet_routes_admin::RuleSetError),
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
    async fn handle_request(
        &self,
        request: RuleSetRequest<I>,
    ) -> Result<ControlFlow<I::RuleSetControlHandle>, fidl::Error> {
        match request {
            RuleSetRequest::AddRule { index, selector, action, responder } => {
                let priority = self.priority;
                let selector = match selector {
                    Ok(selector) => selector,
                    Err(err) => {
                        log::warn!("error addding a rule: {err:?}");
                        return responder
                            .send(Err(fnet_routes_admin::RuleSetError::BaseSelectorMissing))
                            .map(ControlFlow::Continue);
                    }
                };
                let result =
                    self.apply_rule_op(RuleOp::Add { priority, index, selector, action }).await;
                ApplyRuleOpError::respond_result_with(result, responder)
            }
            RuleSetRequest::RemoveRule { index, responder } => {
                let priority = self.priority;

                let result = self.apply_rule_op(RuleOp::Remove { priority, index }).await;
                ApplyRuleOpError::respond_result_with(result, responder)
            }
            RuleSetRequest::AuthenticateForInterface { credential: _, responder } => {
                // TODO(https://fxbug.dev/345315995): Implement authentication.
                responder
                    .send(Err(fnet_routes_admin::RuleSetError::BadAuthentication))
                    .map(ControlFlow::Continue)
            }
            RuleSetRequest::AuthenticateForRouteTable { table: _, token: _, responder } => {
                // TODO(https://fxbug.dev/345315995): Implement authentication.
                responder
                    .send(Err(fnet_routes_admin::RuleSetError::BadAuthentication))
                    .map(ControlFlow::Continue)
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
    set: UserRuleSet<I>,
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
                        let rule_set = UserRuleSet { rule_work_sink, priority };
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
