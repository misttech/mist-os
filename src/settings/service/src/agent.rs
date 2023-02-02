// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base::SettingType;
use crate::event;
use crate::message::base::MessengerType;
use crate::payload_convert;
use crate::policy::PolicyType;
use crate::service;
use crate::service::message::Receptor;
use crate::service_context::ServiceContext;

use futures::future::BoxFuture;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;

/// Agent for watching the camera3 status.
pub(crate) mod camera_watcher;

/// Agent for handling media button input.
pub(crate) mod media_buttons;

/// This mod provides a concrete implementation of the agent authority.
pub(crate) mod authority;

/// Agent for rehydrating actions for restore.
pub(crate) mod restore_agent;

/// Agent for managing access to storage.
pub(crate) mod storage_agent;

/// Earcons.
pub(crate) mod earcons;

/// Inspect agents.
pub(crate) mod inspect;

#[derive(Error, Debug, Clone, Copy, PartialEq)]
pub enum AgentError {
    #[error("Unhandled Lifespan")]
    UnhandledLifespan,
    #[error("Unexpected Error")]
    UnexpectedError,
}

pub(crate) type InvocationResult = Result<(), AgentError>;

/// The scope of an agent's life. Initialization components should
/// only run at the beginning of the service. Service components follow
/// initialization and run for the duration of the service.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Lifespan {
    Initialization,
    Service,
}

/// Struct of information passed to the agent during each invocation.
#[derive(Clone)]
pub struct Invocation {
    pub(crate) lifespan: Lifespan,
    pub(crate) service_context: Arc<ServiceContext>,
}

impl Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invocation").field("lifespan", &self.lifespan).finish_non_exhaustive()
    }
}

impl PartialEq for Invocation {
    fn eq(&self, other: &Self) -> bool {
        self.lifespan == other.lifespan
    }
}

pub type AgentFuture = BoxFuture<'static, ()>;

/// Supported types of [Agent] creation functions.
pub enum CreationFunc {
    /// A simple wrapper around a static creation function.
    ///
    /// Example usage:
    /// ```no_run
    /// async fn create(c: Context) { }
    /// let f = CreationFunc::Static(|c| Box::Pin(example::create(c));
    /// ```
    Static(fn(Context) -> AgentFuture),
    /// Used for more complicate creation functions that need to capture state.
    ///
    /// Example usage:
    /// ```no_run
    /// let shared = Arc::new(Mutex::new(0u));
    /// let f = CreationFunc::Dynamic(Arc::new(move |c| -> AgentFuture {
    ///     let shared = shared.clone();
    ///     Box::pin(async move {
    ///         *shared.lock().await = 42;
    ///     })
    /// }));
    /// ```
    Dynamic(Arc<dyn Fn(Context) -> AgentFuture>),
}

/// AgentCreator provides a simple wrapper for an async Agent creation function.
pub struct AgentCreator {
    pub debug_id: &'static str,
    pub create: CreationFunc,
}

impl AgentCreator {
    pub(crate) fn create(&self, context: Context) -> AgentFuture {
        match &self.create {
            CreationFunc::Static(f) => (f)(context),
            CreationFunc::Dynamic(f) => (f)(context),
        }
    }
}

/// Agent Context contains necessary parts to create an agent.
pub struct Context {
    /// The receivor end to receive messages.
    pub receptor: Receptor,

    /// Publishers are used to publish events.
    publisher: event::Publisher,

    /// Delegates are used to create new messengers.
    pub delegate: service::message::Delegate,

    /// Indicates available Settings interfaces.
    pub(crate) available_components: HashSet<SettingType>,

    /// Indicates available policy types supported by the Settings interfaces.
    pub available_policies: HashSet<PolicyType>,
}

impl Context {
    pub(crate) async fn new(
        receptor: Receptor,
        delegate: service::message::Delegate,
        available_components: HashSet<SettingType>,
        available_policies: HashSet<PolicyType>,
    ) -> Self {
        let publisher = event::Publisher::create(&delegate, MessengerType::Unbound).await;
        Self { receptor, publisher, delegate, available_components, available_policies }
    }

    /// Generates a new `Messenger` on the service `MessageHub`. Only
    /// top-level messages can be sent, not received, as the associated
    /// `Receptor` is discarded.
    async fn create_messenger(
        &self,
    ) -> Result<service::message::Messenger, service::message::MessageError> {
        Ok(self.delegate.create(MessengerType::Unbound).await?.0)
    }

    pub(crate) fn get_publisher(&self) -> event::Publisher {
        self.publisher.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    Invocation(Invocation),
    Complete(InvocationResult),
}

payload_convert!(Agent, Payload);
