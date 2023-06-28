// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing individual clients (aka sockets) of Netlink.

use std::{
    fmt::{Debug, Display, Formatter},
    num::NonZeroU32,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use derivative::Derivative;
use netlink_packet_core::NetlinkMessage;

use crate::{
    logging::{log_debug, log_warn},
    messaging::Sender,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastGroupMemberships,
    },
    protocol_family::ProtocolFamily,
};

/// A unique identifier for a client.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClientId(u64);

/// The aspects of a client shared by [`InternalClient`] and [`ExternalClient`].
struct InnerClient<F: ProtocolFamily> {
    /// The unique ID for this client.
    id: ClientId,
    /// The client's current multicast group memberships.
    group_memberships: Mutex<MulticastGroupMemberships<F>>,
    /// The client's assigned port number.
    port_number: Mutex<Option<NonZeroU32>>,
}

impl<F: ProtocolFamily> Display for InnerClient<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let InnerClient { id: ClientId(id), group_memberships: _, port_number } = self;
        write!(f, "Client[{}/{:?}, {}]", id, *port_number.lock().unwrap(), F::NAME)
    }
}

/// The internal half of a Netlink client, with the external half being provided
/// by ['ExternalClient'].
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct InternalClient<F: ProtocolFamily, S: Sender<F::InnerMessage>> {
    /// The inner client.
    inner: Arc<InnerClient<F>>,
    /// The [`Sender`] of messages from Netlink to the Client.
    sender: S,
}

impl<F: ProtocolFamily, S: Sender<F::InnerMessage>> Debug for InternalClient<F, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl<F: ProtocolFamily, S: Sender<F::InnerMessage>> Display for InternalClient<F, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let InternalClient { inner, sender: _ } = self;
        write!(f, "{}", inner)
    }
}

impl<F: ProtocolFamily, S: Sender<F::InnerMessage>> InternalClient<F, S> {
    /// Returns true if this client is a member of the provided group.
    pub(crate) fn member_of_group(&self, group: ModernGroup) -> bool {
        self.inner.group_memberships.lock().unwrap().member_of_group(group)
    }

    /// Sends the given unicast message to the external half of this client.
    pub(crate) fn send_unicast(&mut self, message: NetlinkMessage<F::InnerMessage>) {
        self.send(message, None)
    }

    /// Sends the given multicast message to the external half of this client.
    fn send_multicast(&mut self, message: NetlinkMessage<F::InnerMessage>, group: ModernGroup) {
        self.send(message, Some(group))
    }

    fn send(&mut self, mut message: NetlinkMessage<F::InnerMessage>, group: Option<ModernGroup>) {
        if let Some(port_number) = *self.inner.port_number.lock().unwrap() {
            message.header.port_number = port_number.into();
        }
        self.sender.send(message, group)
    }
}

/// The external half of a Netlink client, with the internal half being provided
/// by ['InternalClient'].
pub(crate) struct ExternalClient<F: ProtocolFamily> {
    /// The inner client.
    inner: Arc<InnerClient<F>>,
}

impl<F: ProtocolFamily> Display for ExternalClient<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let ExternalClient { inner } = self;
        write!(f, "{}", inner)
    }
}

impl<F: ProtocolFamily> ExternalClient<F> {
    pub(crate) fn set_port_number(&self, v: NonZeroU32) {
        *self.inner.port_number.lock().unwrap() = Some(v);
    }

    /// Adds the given multicast group membership.
    pub(crate) fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        let res = self.inner.group_memberships.lock().unwrap().add_membership(group);
        match res {
            Ok(()) => log_debug!("{} joined multicast group: {:?}", self, group),
            Err(InvalidModernGroupError) => {
                log_warn!("{} failed to join invalid multicast group: {:?}", self, group)
            }
        };
        res
    }

    /// Deletes the given multicast group membership.
    pub(crate) fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
        let res = self.inner.group_memberships.lock().unwrap().del_membership(group);
        match res {
            Ok(()) => log_debug!("{} left multicast group: {:?}", self, group),
            Err(InvalidModernGroupError) => {
                log_warn!("{} failed to leave invalid multicast group: {:?}", self, group)
            }
        };
        res
    }

    /// Sets the legacy multicast group memberships.
    pub(crate) fn set_legacy_memberships(
        &self,
        legacy_memberships: LegacyGroups,
    ) -> Result<(), InvalidLegacyGroupsError> {
        let res =
            self.inner.group_memberships.lock().unwrap().set_legacy_memberships(legacy_memberships);
        match res {
            Ok(()) => log_debug!("{} updated multicast groups: {:?}", self, legacy_memberships),
            Err(InvalidLegacyGroupsError) => {
                log_warn!("{} failed to update multicast groups: {:?}", self, legacy_memberships)
            }
        };
        res
    }
}

// Instantiate a new client pair.
pub(crate) fn new_client_pair<F: ProtocolFamily, S: Sender<F::InnerMessage>>(
    id: ClientId,
    sender: S,
) -> (ExternalClient<F>, InternalClient<F, S>) {
    let inner = Arc::new(InnerClient {
        id,
        group_memberships: Mutex::new(MulticastGroupMemberships::new()),
        port_number: Mutex::default(),
    });
    (ExternalClient { inner: inner.clone() }, InternalClient { inner, sender: sender })
}

/// A generator of [`ClientId`].
#[derive(Default)]
pub(crate) struct ClientIdGenerator(AtomicU64);

impl ClientIdGenerator {
    /// Returns a unique [`ClientId`].
    pub(crate) fn new_id(&self) -> ClientId {
        let ClientIdGenerator(next_id) = self;
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        assert_ne!(id, u64::MAX, "exhausted client IDs");
        ClientId(id)
    }
}

/// The table of connected clients for a given ProtocolFamily.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Default(bound = ""))]
pub(crate) struct ClientTable<F: ProtocolFamily, S: Sender<F::InnerMessage>> {
    clients: Arc<Mutex<Vec<InternalClient<F, S>>>>,
}

impl<F: ProtocolFamily, S: Sender<F::InnerMessage>> ClientTable<F, S> {
    /// Adds the given client to this [`ClientTable`].
    pub(crate) fn add_client(&self, client: InternalClient<F, S>) {
        self.clients.lock().unwrap().push(client);
    }

    /// Sends the message to all clients who are members of the multicast group.
    pub(crate) fn send_message_to_group(
        &self,
        message: NetlinkMessage<F::InnerMessage>,
        group: ModernGroup,
    ) {
        let count = self.clients.lock().unwrap().iter_mut().fold(0, |count, client| {
            if client.member_of_group(group) {
                client.send_multicast(message.clone(), group);
                count + 1
            } else {
                count
            }
        });
        log_debug!(
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

    pub(crate) const CLIENT_ID_1: ClientId = ClientId(1);
    pub(crate) const CLIENT_ID_2: ClientId = ClientId(2);
    pub(crate) const CLIENT_ID_3: ClientId = ClientId(2);
    pub(crate) const CLIENT_ID_4: ClientId = ClientId(4);
    pub(crate) const CLIENT_ID_5: ClientId = ClientId(5);

    /// Creates a new client with memberships to the given groups.
    pub(crate) fn new_fake_client<F: ProtocolFamily>(
        id: ClientId,
        group_memberships: &[ModernGroup],
    ) -> (FakeSenderSink<F::InnerMessage>, InternalClient<F, FakeSender<F::InnerMessage>>) {
        let (sender, sender_sink) = crate::messaging::testutil::fake_sender_with_sink();
        let (external_client, internal_client) = new_client_pair(id, sender);
        for group in group_memberships {
            external_client.add_membership(*group).expect("add group membership");
        }
        (sender_sink, internal_client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use assert_matches::assert_matches;
    use test_case::test_case;

    use crate::{
        messaging::testutil::{fake_sender_with_sink, FakeSender, SentMessage},
        protocol_family::testutil::{
            new_fake_netlink_message, FakeProtocolFamily, MODERN_GROUP1, MODERN_GROUP2,
        },
    };

    // Verify that multicast group membership changes applied to the external
    // client are observed on the internal client.
    #[test]
    fn test_group_memberships() {
        let (external_client, internal_client) =
            new_client_pair::<FakeProtocolFamily, _>(testutil::CLIENT_ID_1, FakeSender::default());

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
        let (mut sink_group1, client_group1) = testutil::new_fake_client::<FakeProtocolFamily>(
            testutil::CLIENT_ID_1,
            &[MODERN_GROUP1],
        );
        let (mut sink_group2, client_group2) = testutil::new_fake_client::<FakeProtocolFamily>(
            testutil::CLIENT_ID_2,
            &[MODERN_GROUP2],
        );
        let (mut sink_both_groups, client_both_groups) =
            testutil::new_fake_client::<FakeProtocolFamily>(
                testutil::CLIENT_ID_3,
                &[MODERN_GROUP1, MODERN_GROUP2],
            );
        clients.add_client(client_group1);
        clients.add_client(client_group2);
        clients.add_client(client_both_groups);

        assert_eq!(&sink_group1.take_messages()[..], &[]);
        assert_eq!(&sink_group2.take_messages()[..], &[]);
        assert_eq!(&sink_both_groups.take_messages()[..], &[]);

        clients.send_message_to_group(new_fake_netlink_message(), MODERN_GROUP1);
        assert_eq!(
            &sink_group1.take_messages()[..],
            &[SentMessage::multicast(new_fake_netlink_message(), MODERN_GROUP1)]
        );
        assert_eq!(&sink_group2.take_messages()[..], &[]);
        assert_eq!(
            &sink_both_groups.take_messages()[..],
            &[SentMessage::multicast(new_fake_netlink_message(), MODERN_GROUP1)]
        );

        clients.send_message_to_group(new_fake_netlink_message(), MODERN_GROUP2);
        assert_eq!(&sink_group1.take_messages()[..], &[]);
        assert_eq!(
            &sink_group2.take_messages()[..],
            &[SentMessage::multicast(new_fake_netlink_message(), MODERN_GROUP2)]
        );
        assert_eq!(
            &sink_both_groups.take_messages()[..],
            &[SentMessage::multicast(new_fake_netlink_message(), MODERN_GROUP2)]
        );
    }

    #[test]
    fn test_client_id_generator() {
        let generator = ClientIdGenerator::default();
        const NUM_IDS: u32 = 1000;
        let mut ids = HashSet::new();
        for _ in 0..NUM_IDS {
            // `insert` returns false if the ID is already present in the set,
            // indicating that the generator returned a non-unique ID.
            assert!(ids.insert(generator.new_id()));
        }
    }

    #[test_case(0; "pid_0")]
    #[test_case(1; "pid_1")]
    fn test_set_port_number(port_number: u32) {
        let (sender, mut sink) = fake_sender_with_sink();
        let (external_client, mut internal_client) =
            new_client_pair::<FakeProtocolFamily, _>(testutil::CLIENT_ID_1, sender);

        if let Some(port_number) = NonZeroU32::new(port_number) {
            external_client.set_port_number(port_number)
        }

        let message = new_fake_netlink_message();
        assert_eq!(message.header.port_number, 0);
        internal_client.send_unicast(message);

        assert_matches!(
            &sink.take_messages()[..],
            [SentMessage { message, group }] => {
                assert_eq!(message.header.port_number, port_number);
                assert_eq!(*group, None);
            }
        )
    }
}
