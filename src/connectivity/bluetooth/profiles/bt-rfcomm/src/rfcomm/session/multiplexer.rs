// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bt_rfcomm::frame::Frame;
use bt_rfcomm::{RfcommError, Role, DLCI};
use fuchsia_bluetooth::types::Channel;
use fuchsia_inspect as inspect;
use fuchsia_inspect_derive::Inspect;
use futures::channel::mpsc;
use std::collections::HashMap;
use tracing::{info, trace, warn};

use crate::rfcomm::inspect::SessionMultiplexerInspect;
use crate::rfcomm::session::channel::{FlowControlMode, FlowControlledData, SessionChannel};
use crate::rfcomm::types::Error;

/// The parameters associated with this Session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SessionParameters {
    /// Whether credit-based flow control is being used for this session.
    pub credit_based_flow: bool,

    /// The max MTU size of an RFCOMM frame.
    pub max_frame_size: u16,
}

impl SessionParameters {
    fn default_preferred(max_frame_size: u16) -> Self {
        // By default, we prefer to use credit flow control.
        SessionParameters { max_frame_size, credit_based_flow: true }
    }

    /// Combines the current session parameters with the `other` parameters and returns
    /// a negotiated SessionParameters.
    fn negotiate(&self, other: &SessionParameters) -> Self {
        // Our implementation is OK with credit based flow. We choose whatever the new
        // configuration requests.
        let credit_based_flow = other.credit_based_flow;
        // Use the smaller, more restrictive, max frame size.
        let max_frame_size = std::cmp::min(self.max_frame_size, other.max_frame_size);
        Self { credit_based_flow, max_frame_size }
    }
}

/// The current state of the session parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParameterNegotiationState {
    /// Parameters have not been negotiated - contains our preferred `SessionParameters`
    NotNegotiated(SessionParameters),
    /// Parameters have been negotiated.
    Negotiated(SessionParameters),
}

impl ParameterNegotiationState {
    /// Returns the current session parameters.
    /// If not negotiated, then the default preferred parameters is returned.
    fn parameters(&self) -> SessionParameters {
        match self {
            Self::Negotiated(params) => *params,
            Self::NotNegotiated(params) => *params,
        }
    }

    /// Negotiates the `new` parameters with the (potential) current parameters. Returns
    /// the parameters that were set.
    fn negotiate(&mut self, new: SessionParameters) -> SessionParameters {
        let updated = self.parameters().negotiate(&new);
        *self = Self::Negotiated(updated);
        updated
    }
}

/// The `SessionMultiplexer` manages channels over the range of valid User-DLCIs. It is responsible
/// for maintaining the current state of the RFCOMM Session, and provides an API to create,
/// establish, and relay user data over the multiplexed channels.
///
/// The `SessionMultiplexer` is considered "started" when its Role has been assigned.
/// The parameters for the multiplexer must be negotiated before the first DLCI has
/// been established. RFCOMM 5.5.3 states that renegotiation of parameters is optional - this
/// multiplexer will simply echo the current parameters in the event a negotiation request is
/// received after the first DLC is opened and established.
#[derive(Inspect)]
pub struct SessionMultiplexer {
    /// The role for the multiplexer.
    role: Role,
    /// The maximum RFCOMM packet size that can be sent over the underlying transport.
    max_rfcomm_packet_size: u16,
    /// The parameters for the multiplexer.
    parameters: ParameterNegotiationState,
    /// Local opened RFCOMM channels for this session.
    channels: HashMap<DLCI, SessionChannel>,
    /// The inspect node for this object.
    #[inspect(forward)]
    inspect: SessionMultiplexerInspect,
}

impl SessionMultiplexer {
    pub fn create(max_rfcomm_packet_size: u16) -> Self {
        Self {
            role: Role::Unassigned,
            max_rfcomm_packet_size,
            parameters: ParameterNegotiationState::NotNegotiated(
                SessionParameters::default_preferred(max_rfcomm_packet_size),
            ),
            channels: HashMap::new(),
            inspect: SessionMultiplexerInspect::default(),
        }
    }

    /// Resets the multiplexer back to its initial state with no opened channels.
    pub fn reset(&mut self) {
        *self = Self::create(self.max_rfcomm_packet_size);
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn set_role(&mut self, role: Role) {
        self.role = role;
        self.inspect.set_role(role);
    }

    /// Returns true if credit-based flow control is enabled.
    pub fn credit_based_flow(&self) -> bool {
        self.parameters().credit_based_flow
    }

    #[cfg(test)]
    pub fn parameter_negotiation_state(&self) -> ParameterNegotiationState {
        self.parameters
    }

    /// Returns true if the session parameters have been negotiated.
    pub fn parameters_negotiated(&self) -> bool {
        std::matches!(&self.parameters, ParameterNegotiationState::Negotiated(_))
    }

    /// Returns the parameters associated with this session.
    pub fn parameters(&self) -> SessionParameters {
        self.parameters.parameters()
    }

    /// Negotiates the parameters for this session - returns the session parameters that were set.
    pub fn negotiate_parameters(
        &mut self,
        new_session_parameters: SessionParameters,
    ) -> SessionParameters {
        // The session parameters can only be modified if no DLCs have been established.
        if self.dlc_established() {
            warn!(
                "Received negotiation request when at least one DLC has already been established"
            );
            return self.parameters();
        }

        // Otherwise, it is OK to negotiate the multiplexer parameters.
        let updated = self.parameters.negotiate(new_session_parameters);
        trace!("Updated Session parameters: {:?}", updated);
        self.inspect.set_parameters(updated);
        updated
    }

    /// Returns true if the multiplexer has started.
    pub fn started(&self) -> bool {
        self.role.is_multiplexer_started()
    }

    /// Starts the session multiplexer and assumes the provided `role`. Returns Ok(()) if mux
    /// startup is successful.
    pub fn start(&mut self, role: Role) -> Result<(), Error> {
        // Re-starting the multiplexer is not valid, as this would invalidate any opened
        // RFCOMM channels.
        if self.started() {
            return Err(Error::MultiplexerAlreadyStarted);
        }

        // Role must be a valid started role.
        if !role.is_multiplexer_started() {
            return Err(RfcommError::InvalidRole(role).into());
        }

        self.set_role(role);
        info!(?role, "RFCOMM Session multiplexer started");
        Ok(())
    }

    /// Returns true if the provided `dlci` has been initialized and established in
    /// the multiplexer.
    pub fn dlci_established(&self, dlci: &DLCI) -> bool {
        self.channels.get(dlci).map(|c| c.is_established()).unwrap_or(false)
    }

    /// Returns true if at least one DLC has been established.
    pub fn dlc_established(&self) -> bool {
        self.channels
            .iter()
            .fold(false, |acc, (_, session_channel)| acc | session_channel.is_established())
    }

    /// Finds or initializes a new SessionChannel for the provided `dlci`. Returns a mutable
    /// reference to the channel.
    pub fn find_or_create_session_channel(&mut self, dlci: DLCI) -> &mut SessionChannel {
        let channel = self.channels.entry(dlci).or_insert_with(|| {
            let mut channel = SessionChannel::new(dlci, self.role);
            let _ = channel.iattach(self.inspect.node(), inspect::unique_name("channel_"));
            channel
        });
        channel
    }

    /// Returns true if the parameters have been negotiated for the provided `dlci`.
    pub fn dlc_parameters_negotiated(&self, dlci: &DLCI) -> bool {
        self.channels.get(dlci).is_some_and(|c| c.parameters_negotiated())
    }

    /// Sets the flow control mode of the RFCOMM channel associated with the `dlci` to use
    /// the provided `flow_control`.
    /// Returns an Error if the DLCI is not registered, or if the SessionChannel has already
    /// been established.
    pub fn set_flow_control(
        &mut self,
        dlci: DLCI,
        flow_control: FlowControlMode,
    ) -> Result<(), Error> {
        self.channels.get_mut(&dlci).map_or(Err(RfcommError::InvalidDLCI(dlci).into()), |channel| {
            channel.set_flow_control(flow_control)
        })
    }

    /// Attempts to establish a SessionChannel for the provided `dlci`.
    /// `user_data_sender` is used by the SessionChannel to relay any received UserData
    /// frames from the client associated with the channel.
    ///
    /// Returns the remote end of the channel on success.
    pub fn establish_session_channel(
        &mut self,
        dlci: DLCI,
        user_data_sender: mpsc::Sender<Frame>,
    ) -> Result<Channel, Error> {
        // If the session parameters have not been negotiated, set them to our preferred default.
        if !self.parameters_negotiated() {
            let _ = self.negotiate_parameters(self.parameters());
        }

        // Potentially reserve a new `SessionChannel` for the provided DLCI - the max TX for this
        // channel is the negotiated `max_frame_size`.
        let max_tx_size = self.parameters().max_frame_size;
        let channel = self.find_or_create_session_channel(dlci);
        if channel.is_established() {
            return Err(Error::ChannelAlreadyEstablished(dlci));
        }

        // Create endpoints for the session channel. The local end is held by this component
        // and the remote end is returned to be held by a RFCOMM profile.
        let (local, remote) = Channel::create_with_max_tx(max_tx_size.into());
        channel.establish(local, user_data_sender);
        Ok(remote)
    }

    /// Closes the SessionChannel for the provided `dlci`. Returns true if the SessionChannel
    /// was closed.
    pub fn close_session_channel(&mut self, dlci: &DLCI) -> bool {
        self.channels.remove(dlci).is_some()
    }

    /// Sends `user_data` received from the peer to the SessionChannel associated with the `dlci`.
    pub async fn receive_user_data(
        &mut self,
        dlci: DLCI,
        user_data: FlowControlledData,
    ) -> Result<(), Error> {
        if let Some(session_channel) = self.channels.get_mut(&dlci) {
            return session_channel.receive_user_data(user_data).await;
        }
        Err(RfcommError::InvalidDLCI(dlci).into())
    }
}

// TODO(https://fxbug.dev/42140263): IWBN to have focused tests for the `SessionMultiplexer`. Currently, it's
// transitively tested by the tests for `SessionInner`.
#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;

    use crate::rfcomm::inspect::CREDIT_FLOW_CONTROL;

    #[test]
    fn test_multiplexer_inspect() {
        let mut exec = fuchsia_async::TestExecutor::new();
        let inspect = inspect::Inspector::default();

        // Setup multiplexer with inspect.
        let mut multiplexer = SessionMultiplexer::create(600);
        multiplexer.iattach(inspect.root(), "multiplexer").expect("should attach to inspect tree");
        // Default inspect tree.
        assert_data_tree!(inspect, root: {
            multiplexer: {
                role: "Unassigned",
            },
        });

        // Reserving a channel should add to the inspect tree.
        let dlci = DLCI::try_from(9).unwrap();
        let _ = multiplexer.find_or_create_session_channel(dlci);
        assert_data_tree!(inspect, root: {
            multiplexer: {
                role: "Unassigned",
                channel_0: contains {
                    dlci: 9u64,
                }
            },
        });

        // Establishing a channel should add to the inspect tree. Multiplexer parameters are
        // negotiated to a default and updated in the inspect tree.
        let dlci2 = DLCI::try_from(20).unwrap();
        let (sender2, _receiver2) = mpsc::channel(0);
        let _channel2 = multiplexer.establish_session_channel(dlci2, sender2);
        assert_data_tree!(inspect, root: {
            multiplexer: {
                role: "Unassigned",
                flow_control: CREDIT_FLOW_CONTROL,
                max_frame_size: 600u64,
                channel_0: contains {
                    dlci: 9u64,
                },
                channel_1: contains {
                    dlci: 20u64,
                }
            },
        });

        // Removing a channel is OK. The lifetime of the `channel_*` node is tied to the
        // SessionChannel. This makes cleanup easy.
        assert!(multiplexer.close_session_channel(&dlci2));
        // The multiplexer closing the SessionChannel results in dropping the fasync::Task<()>
        // for the channel. In doing so, the RemoteHandle for the Task is dropped. The
        // associated future will only then be _woken up_ to be dropped by the executor.
        // This line of code runs the executor to complete the drop of the future. Only then
        // will the `channel_1` inspect node be removed from the tree.
        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        assert_data_tree!(inspect, root: {
            multiplexer: {
                role: "Unassigned",
                flow_control: CREDIT_FLOW_CONTROL,
                max_frame_size: 600u64,
                channel_0: contains {
                    dlci: 9u64,
                },
            },
        });
    }
}
