// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use diagnostics_traits::{InspectableValue, Inspector};

use crate::parse::{IncomingResponseToRequestErrorCounters, SelectingIncomingMessageErrorCounters};

/// An incrementing counter.
#[derive(Default, Debug)]
pub(crate) struct Counter(AtomicUsize);

impl Counter {
    pub(crate) fn increment(&self) {
        let _: usize = self.0.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn load(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl InspectableValue for Counter {
    fn record<I: diagnostics_traits::Inspector>(&self, name: &str, inspector: &mut I) {
        inspector.record_uint(name, u64::try_from(self.load()).unwrap_or(u64::MAX));
    }
}

pub(crate) fn record_optional_duration_secs(
    inspector: &mut impl Inspector,
    name: &str,
    value: Option<Duration>,
) {
    match value {
        Some(value) => inspector.record_uint(name, value.as_secs()),
        None => inspector.record_display(name, "Unset"),
    }
}

/// Counters relating to sending and receiving messages.
#[derive(Debug, Default)]
pub(crate) struct MessagingRelatedCounters {
    /// A counter for each time the client sent a message.
    pub(crate) send_message: Counter,
    /// A counter for each time the client received a message.
    pub(crate) recv_message: Counter,
    /// A counter for each time the client observed a fatal socket error while
    /// trying to receive a message.
    pub(crate) recv_message_fatal_socket_error: Counter,
    /// A counter for each time the client observed a non-fatal socket error
    /// while trying to receive a message.
    pub(crate) recv_message_non_fatal_socket_error: Counter,
    /// A counter for each time the client has timed out waiting to receive
    /// (causing it to retransmit or state-transition).
    pub(crate) recv_time_out: Counter,
    /// A counter for each time the client received a message with the wrong
    /// transaction ID.
    pub(crate) recv_wrong_xid: Counter,
    /// A counter for each time the client received a message specifying an
    /// incorrect chaddr (client hardware address).
    pub(crate) recv_wrong_chaddr: Counter,
    /// A counter for each time the client received a UDPv4 packet on the
    /// dhcp-client port that did not parse as a DHCP message.
    pub(crate) recv_failed_dhcp_parse: Counter,
}

impl MessagingRelatedCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self {
            send_message,
            recv_message,
            recv_message_fatal_socket_error,
            recv_message_non_fatal_socket_error,
            recv_time_out,
            recv_wrong_xid,
            recv_wrong_chaddr,
            recv_failed_dhcp_parse,
        } = self;
        inspector.record_inspectable_value("SendMessage", send_message);
        inspector.record_inspectable_value("RecvMessage", recv_message);
        inspector.record_inspectable_value(
            "RecvMessageFatalSocketError",
            recv_message_fatal_socket_error,
        );
        inspector.record_inspectable_value(
            "RecvMessageNonFatalSocketError",
            recv_message_non_fatal_socket_error,
        );
        inspector.record_inspectable_value("RecvTimeOut", recv_time_out);
        inspector.record_inspectable_value("RecvWrongXid", recv_wrong_xid);
        inspector.record_inspectable_value("RecvWrongChaddr", recv_wrong_chaddr);
        inspector.record_inspectable_value("RecvFailedDhcpParse", recv_failed_dhcp_parse);
    }
}

/// Counters for the Init state.
#[derive(Debug, Default)]
pub(crate) struct InitCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
}

impl InitCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered } = self;
        inspector.record_inspectable_value("Entered", entered);
    }
}

/// Counters for the Selecting state.
#[derive(Debug, Default)]
pub(crate) struct SelectingCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
    /// Counters relating to sending and receiving messages.
    pub(crate) messaging: MessagingRelatedCounters,
    /// Counters for each error that could cause the client to reject a message
    /// while receiving in the Selecting state.
    pub(crate) recv_error: SelectingIncomingMessageErrorCounters,
}

impl SelectingCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered, messaging, recv_error } = self;
        inspector.record_inspectable_value("Entered", entered);
        messaging.record(inspector);
        recv_error.record(inspector);
    }
}

/// Counters for the Requesting state.
#[derive(Debug, Default)]
pub(crate) struct RequestingCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
    /// Counters relating to sending and receiving messages.
    pub(crate) messaging: MessagingRelatedCounters,
    /// Counters for each error that could cause a client to reject a message
    /// while receiving in the Requesting state.
    pub(crate) recv_error: IncomingResponseToRequestErrorCounters,
    /// Counter for each time the client received a NAK message while in the
    /// Requesting state.
    pub(crate) recv_nak: Counter,
}

impl RequestingCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered, messaging, recv_error, recv_nak } = self;
        inspector.record_inspectable_value("Entered", entered);
        messaging.record(inspector);
        recv_error.record(inspector);
        inspector.record_inspectable_value("RecvNak", recv_nak);
    }
}

/// Counters for the Bound state.
#[derive(Debug, Default)]
pub(crate) struct BoundCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
}

impl BoundCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered } = self;
        inspector.record_inspectable_value("Entered", entered);
    }
}

/// Counters for the Renewing state.
#[derive(Debug, Default)]
pub(crate) struct RenewingCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
    /// Counters relating to sending and receiving messages.
    pub(crate) messaging: MessagingRelatedCounters,
    /// Counters for each error that could cause a client to reject a message
    /// while receiving in the Requesting state.
    pub(crate) recv_error: IncomingResponseToRequestErrorCounters,
    /// Counter for each time the client received a NAK message while in the
    /// Renewing state.
    pub(crate) recv_nak: Counter,
}

impl RenewingCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered, messaging, recv_error, recv_nak } = self;
        inspector.record_inspectable_value("Entered", entered);
        messaging.record(inspector);
        recv_error.record(inspector);
        inspector.record_inspectable_value("RecvNak", recv_nak);
    }
}

/// Counters for the Rebinding state.
#[derive(Debug, Default)]
pub(crate) struct RebindingCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
    /// Counters relating to sending and receiving messages.
    pub(crate) messaging: MessagingRelatedCounters,
    /// Counters for each error that could cause a client to reject a message
    /// while receiving in the Rebinding state.
    pub(crate) recv_error: IncomingResponseToRequestErrorCounters,
    /// Counter for each time the client received a NAK message while in the
    /// Rebinding state.
    pub(crate) recv_nak: Counter,
}

impl RebindingCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered, messaging, recv_error, recv_nak } = self;
        inspector.record_inspectable_value("Entered", entered);
        messaging.record(inspector);
        recv_error.record(inspector);
        inspector.record_inspectable_value("RecvNak", recv_nak);
    }
}

/// Counters for the WaitingToRestart state.
#[derive(Debug, Default)]
pub(crate) struct WaitingToRestartCounters {
    /// The number of times the state was entered.
    pub(crate) entered: Counter,
}

impl WaitingToRestartCounters {
    fn record(&self, inspector: &mut impl Inspector) {
        let Self { entered } = self;
        inspector.record_inspectable_value("Entered", entered);
    }
}

/// Debugging counters for the core state machine.
#[derive(Default, Debug)]
pub struct Counters {
    pub(crate) init: InitCounters,
    pub(crate) selecting: SelectingCounters,
    pub(crate) requesting: RequestingCounters,
    pub(crate) bound: BoundCounters,
    pub(crate) renewing: RenewingCounters,
    pub(crate) rebinding: RebindingCounters,
    pub(crate) waiting_to_restart: WaitingToRestartCounters,
}

impl Counters {
    /// Records the counters in the given [`Inspector`].
    pub fn record(&self, inspector: &mut impl Inspector) {
        let Self { init, selecting, requesting, bound, renewing, rebinding, waiting_to_restart } =
            self;
        inspector.record_child("Init", |inspector| {
            init.record(inspector);
        });
        inspector.record_child("Selecting", |inspector| {
            selecting.record(inspector);
        });
        inspector.record_child("Requesting", |inspector| {
            requesting.record(inspector);
        });
        inspector.record_child("Bound", |inspector| {
            bound.record(inspector);
        });
        inspector.record_child("Renewing", |inspector| {
            renewing.record(inspector);
        });
        inspector.record_child("Rebinding", |inspector| {
            rebinding.record(inspector);
        });
        inspector.record_child("WaitingToRestart", |inspector| {
            waiting_to_restart.record(inspector);
        });
    }
}
