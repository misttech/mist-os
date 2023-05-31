// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing individual clients (aka sockets) of Netlink.

use std::sync::{Arc, Mutex};

use derivative::Derivative;
use tracing::debug;

use crate::{
    messaging::Sender,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastGroupMemberships,
    },
    protocol_family::ProtocolFamily,
    NETLINK_LOG_TAG,
};

/// The internal half of a Netlink client, with the external half being provided
/// by ['ExternalClient'].
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct InternalClient<F: ProtocolFamily, S: Sender<F::Message>> {
    /// The client's current multicast group memberships.
    group_memberships: Arc<Mutex<MulticastGroupMemberships<F>>>,
    /// The [`Sender`] of messages from Netlink to the Client.
    sender: S,
}

impl<F: ProtocolFamily, S: Sender<F::Message>> InternalClient<F, S> {
    /// Returns true if this client is a member of the provided group.
    pub(crate) fn member_of_group(&self, group: ModernGroup) -> bool {
        self.group_memberships.lock().unwrap().member_of_group(group)
    }

    /// Sends the given message to the external half of this client.
    pub(crate) fn send(&mut self, message: F::Message) {
        self.sender.send(message)
    }
}

/// The external half of a Netlink client, with the internal half being provided
/// by ['InternalClient'].
pub(crate) struct ExternalClient<F: ProtocolFamily> {
    /// The client's current multicast group memberships
    group_memberships: Arc<Mutex<MulticastGroupMemberships<F>>>,
}

impl<F: ProtocolFamily> ExternalClient<F> {
    /// Adds the given multicast group membership.
    pub(crate) fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        self.group_memberships.lock().unwrap().add_membership(group)
    }

    /// Deletes the given multicast group membership.
    pub(crate) fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        self.group_memberships.lock().unwrap().del_membership(group)
    }

    /// Sets the legacy multicast group memberships.
    pub(crate) fn set_legacy_memberships(
        &self,
        legacy_memberships: LegacyGroups,
    ) -> Result<(), InvalidLegacyGroupsError> {
        self.group_memberships.lock().unwrap().set_legacy_memberships(legacy_memberships)
    }
}

// Instantiate a new client pair.
pub(crate) fn new_client_pair<F: ProtocolFamily, S: Sender<F::Message>>(
    sender: S,
) -> (ExternalClient<F>, InternalClient<F, S>) {
    let group_memberships = Arc::new(Mutex::new(MulticastGroupMemberships::new()));
    (
        ExternalClient { group_memberships: group_memberships.clone() },
        InternalClient { group_memberships, sender: sender },
    )
}

/// The table of connected clients for a given ProtocolFamily.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Default(bound = ""))]
pub(crate) struct ClientTable<F: ProtocolFamily, S: Sender<F::Message>> {
    clients: Arc<Mutex<Vec<InternalClient<F, S>>>>,
}

impl<F: ProtocolFamily, S: Sender<F::Message>> ClientTable<F, S> {
    /// Adds the given client to this [`ClientTable`].
    pub(crate) fn add_client(&self, client: InternalClient<F, S>) {
        self.clients.lock().unwrap().push(client);
    }

    /// Sends the message to all clients who are members of the multicast group.
    pub(crate) fn send_message_to_group(&self, message: F::Message, group: ModernGroup) {
        let count = self.clients.lock().unwrap().iter_mut().fold(0, |count, client| {
            if client.member_of_group(group) {
                client.send(message.clone());
                count + 1
            } else {
                count
            }
        });
        debug!(
            tag = NETLINK_LOG_TAG,
            "Notified {} {} clients of message for group {:?}: {:?}",
            count,
            F::NAME,
            group,
            message
        );
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::{
        messaging::testutil::{FakeSender, FakeSenderSink},
        protocol_family::ProtocolFamily,
    };

    /// Creates a new client with memberships to the given groups.
    pub(crate) fn new_fake_client<F: ProtocolFamily>(
        group_memberships: &[ModernGroup],
    ) -> (FakeSenderSink<F::Message>, InternalClient<F, FakeSender<F::Message>>) {
        let (sender, sender_sink) = crate::messaging::testutil::fake_sender_with_sink();
        let (external_client, internal_client) = new_client_pair(sender);
        for group in group_memberships {
            external_client.add_membership(*group).expect("add group membership");
        }
        (sender_sink, internal_client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        messaging::testutil::FakeSender,
        protocol_family::testutil::{
            FakeNetlinkMessage, FakeProtocolFamily, MODERN_GROUP1, MODERN_GROUP2,
        },
    };

    // Verify that multicast group membership changes applied to the external
    // client are observed on the internal client.
    #[test]
    fn test_group_memberships() {
        let (external_client, internal_client) =
            new_client_pair::<FakeProtocolFamily, _>(FakeSender::default());

        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));

        // Add one membership and verify the other is unaffected.
        external_client.add_membership(MODERN_GROUP1).expect("failed to add membership");
        assert!(internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));
        // Add the second membership.
        external_client.add_membership(MODERN_GROUP2).expect("failed to add membership");
        assert!(internal_client.member_of_group(MODERN_GROUP1));
        assert!(internal_client.member_of_group(MODERN_GROUP2));
        // Delete the first membership and verify the other is unaffected.
        external_client.del_membership(MODERN_GROUP1).expect("failed to del membership");
        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(internal_client.member_of_group(MODERN_GROUP2));
        // Delete the second membership.
        external_client.del_membership(MODERN_GROUP2).expect("failed to del membership");
        assert!(!internal_client.member_of_group(MODERN_GROUP1));
        assert!(!internal_client.member_of_group(MODERN_GROUP2));
    }

    #[test]
    fn test_send_message_to_group() {
        let clients = ClientTable::default();
        let (mut sink_group1, client_group1) =
            testutil::new_fake_client::<FakeProtocolFamily>(&[MODERN_GROUP1]);
        let (mut sink_group2, client_group2) =
            testutil::new_fake_client::<FakeProtocolFamily>(&[MODERN_GROUP2]);
        let (mut sink_both_groups, client_both_groups) =
            testutil::new_fake_client::<FakeProtocolFamily>(&[MODERN_GROUP1, MODERN_GROUP2]);
        clients.add_client(client_group1);
        clients.add_client(client_group2);
        clients.add_client(client_both_groups);

        assert_eq!(&sink_group1.take_messages()[..], &[]);
        assert_eq!(&sink_group2.take_messages()[..], &[]);
        assert_eq!(&sink_both_groups.take_messages()[..], &[]);

        clients.send_message_to_group(FakeNetlinkMessage, MODERN_GROUP1);
        assert_eq!(&sink_group1.take_messages()[..], &[FakeNetlinkMessage]);
        assert_eq!(&sink_group2.take_messages()[..], &[]);
        assert_eq!(&sink_both_groups.take_messages()[..], &[FakeNetlinkMessage]);

        clients.send_message_to_group(FakeNetlinkMessage, MODERN_GROUP2);
        assert_eq!(&sink_group1.take_messages()[..], &[]);
        assert_eq!(&sink_group2.take_messages()[..], &[FakeNetlinkMessage]);
        assert_eq!(&sink_both_groups.take_messages()[..], &[FakeNetlinkMessage]);
    }
}
