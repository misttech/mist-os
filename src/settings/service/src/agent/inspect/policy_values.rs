// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::TryFrom;
use std::sync::Arc;

use crate::agent::{Context, Payload};
use crate::clock;
use crate::message::base::{MessageEvent, MessengerType};
use crate::policy::{self as policy_base, Payload as PolicyPayload, Request, Role};
use crate::service::message::{Audience, MessageClient, Messenger, Signature};
use crate::{service, trace};
use anyhow::{format_err, Error};
use fuchsia_async as fasync;
use fuchsia_inspect::{self as inspect, component, Property};
use fuchsia_inspect_derive::Inspect;
use fuchsia_syslog::fx_log_err;
use futures::StreamExt;
use settings_inspect_utils::managed_inspect_map::ManagedInspectMap;

const INSPECT_NODE_NAME: &str = "policy_values";

/// An agent that listens in on messages sent on the message hub to policy handlers
/// to record their internal state to inspect.
pub(crate) struct PolicyValuesInspectAgent {
    messenger_client: Messenger,
    policy_values: ManagedInspectMap<PolicyValuesInspectInfo>,
}

/// Information about a policy to be written to inspect.
///
/// Inspect nodes and properties are not used, but need to be held as they're deleted from inspect
/// once they go out of scope.
#[derive(Default, Inspect)]
struct PolicyValuesInspectInfo {
    /// Node of this info.
    inspect_node: inspect::Node,

    /// Debug string representation of the state of this policy.
    value: inspect::StringProperty,

    /// Milliseconds since Unix epoch that this policy was modified.
    timestamp: inspect::StringProperty,
}

impl PolicyValuesInspectAgent {
    pub(crate) async fn create(context: Context) {
        Self::create_with_node(
            context,
            component::inspector().root().create_child(INSPECT_NODE_NAME),
        )
        .await;
    }

    /// Create an agent to watch messages to the policy layer and record policy
    /// state to the inspect child node. Agent starts immediately without
    /// calling invocation, but acknowledges the invocation payload to
    /// let the Authority know the agent starts properly.
    async fn create_with_node(context: Context, inspect_node: inspect::Node) {
        // We must take care not to observe all requests as the agent itself can
        // issue messages and block on their response. If another message is
        // passed to the agent during this wait, we will deadlock.
        let (messenger_client, broker_receptor) = match context
            .delegate
            .create(MessengerType::Broker(Arc::new(|message| {
                matches!(message.payload(), service::Payload::Policy(PolicyPayload::Request(_)))
            })))
            .await
        {
            Ok(messenger) => messenger,
            Err(err) => {
                fx_log_err!(
                    "broker listening to only policy requests could not be created: {:?}",
                    err
                );
                return;
            }
        };

        let mut agent = Self {
            messenger_client,
            policy_values: ManagedInspectMap::<PolicyValuesInspectInfo>::with_node(inspect_node),
        };

        fasync::Task::spawn(async move {
            let _ = &context;
            let id = fuchsia_trace::Id::new();
            trace!(id, "policy_values_inspect_agent");
            // Request initial values from all policy handlers.
            let initial_get_receptor = agent.messenger_client.message(
                PolicyPayload::Request(Request::Get).into(),
                Audience::Role(service::Role::Policy(Role::PolicyHandler)),
            );

            let initial_get_fuse = initial_get_receptor.fuse();
            let broker_fuse = broker_receptor.fuse();
            let agent_event = context.receptor.fuse();
            futures::pin_mut!(initial_get_fuse, broker_fuse, agent_event);

            loop {
                futures::select! {
                    initial_get_message = initial_get_fuse.select_next_some() => {
                        trace!(
                            id,
                            "initial get"
                        );
                        // Received a reply to our initial broadcast to all policy handlers asking
                        // for their value.
                        agent.handle_initial_get(initial_get_message).await;
                    }

                    intercepted_message = broker_fuse.select_next_some() => {
                        trace!(
                            id,
                            "intercepted"
                        );
                        // Intercepted a policy request.
                        agent.handle_intercepted_message(intercepted_message).await;
                    }

                    agent_message = agent_event.select_next_some() => {
                        trace!(
                            id,
                            "invocation"
                        );
                        if let MessageEvent::Message(
                                service::Payload::Agent(Payload::Invocation(_invocation)), client)
                                = agent_message {
                            // Since the agent runs at creation, there is no
                            // need to handle state here.
                            let _ = client.reply(Payload::Complete(Ok(())).into());
                        }
                    }

                    // This shouldn't ever be triggered since the inspect agent (and its receptors)
                    // should be active for the duration of the service. This is just a safeguard to
                    // ensure this detached task doesn't run forever if the receptors stop somehow.
                    complete => break,
                }
            }
        })
        .detach();
    }

    /// Handles responses to the initial broadcast by the inspect agent to all policy handlers that
    /// requests their state.
    async fn handle_initial_get(&mut self, message: service::message::MessageEvent) {
        if let MessageEvent::Message(payload, _) = message {
            // Since the order for these events isn't guaranteed, don't overwrite responses obtained
            // after intercepting a request with these initial values.
            if let Err(err) = self.write_response_to_inspect(payload, true).await {
                fx_log_err!("Failed write initial get response to inspect: {:?}", err);
            }
        }
    }

    /// Handles messages seen over the message hub and requests policy state from handlers as
    /// needed.
    async fn handle_intercepted_message(&mut self, message: service::message::MessageEvent) {
        if let MessageEvent::Message(service::Payload::Policy(PolicyPayload::Request(_)), client) =
            message
        {
            // When we see a request to a policy proxy, we assume that the policy will be modified,
            // so we wait for the reply to get the signature of the proxy, then ask the proxy for
            // its latest value.
            match PolicyValuesInspectAgent::watch_reply(client).await {
                Ok(reply_signature) => {
                    if let Err(err) = self.request_and_write_to_inspect(reply_signature).await {
                        fx_log_err!("Failed request value from policy proxy: {:?}", err);
                    }
                }
                Err(err) => {
                    fx_log_err!("Failed to watch reply to request: {:?}", err);
                }
            }
        }
    }

    /// Watches for the reply to a sent message and returns the author of the reply.
    async fn watch_reply(mut client: MessageClient) -> Result<Signature, Error> {
        let mut reply_receptor = client.spawn_observer();

        reply_receptor.next_payload().await.map(|(_, reply_client)| reply_client.get_author())
    }

    /// Requests the policy state from a given signature for a policy handler and records the result
    /// in inspect.
    async fn request_and_write_to_inspect(&mut self, signature: Signature) -> Result<(), Error> {
        // Send the request to the policy proxy.
        let mut send_receptor = self
            .messenger_client
            .message(PolicyPayload::Request(Request::Get).into(), Audience::Messenger(signature));

        // Wait for a response from the policy proxy.
        let (payload, _) = send_receptor.next_payload().await?;

        self.write_response_to_inspect(payload, false).await
    }

    /// Writes a policy payload response to inspect.
    ///
    /// ignore_if_present will silently not write the response to inspect if a value already exists
    /// for the policy.
    async fn write_response_to_inspect(
        &mut self,
        payload: service::Payload,
        ignore_if_present: bool,
    ) -> Result<(), Error> {
        let policy_info = if let Ok(PolicyPayload::Response(Ok(
            policy_base::response::Payload::PolicyInfo(policy_info),
        ))) = PolicyPayload::try_from(payload)
        {
            policy_info
        } else {
            return Err(format_err!("did not receive policy state"));
        };

        // Convert the response to a string for inspect.
        let (policy_name, value) = policy_info.for_inspect();
        let timestamp = clock::inspect_format_now();

        let already_present = self.policy_values.map_mut().contains_key(policy_name);

        let policy = self
            .policy_values
            .get_or_insert_with(policy_name.to_string(), PolicyValuesInspectInfo::default);

        if already_present && ignore_if_present {
            return Ok(());
        }

        policy.timestamp.set(&timestamp);
        policy.value.set(&value);

        Ok(())
    }
}
