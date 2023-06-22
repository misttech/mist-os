// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logging::{log_error, log_info, log_warn};
use futures::{channel::mpsc, future::Future, stream::StreamExt};
use netlink::{messaging::Sender, NETLINK_LOG_TAG};
use netlink_packet_core::{NetlinkHeader, NetlinkMessage, NetlinkPayload};
use netlink_packet_generic::{
    constants::GENL_ID_CTRL,
    ctrl::{
        nlas::{GenlCtrlAttrs, McastGrpAttrs},
        GenlCtrl, GenlCtrlCmd,
    },
    GenlMessage,
};
use std::{collections::HashMap, sync::Mutex};

mod messages;
mod nl80211;

pub use messages::GenericMessage;

const MIN_FAMILY_ID: u16 = GENL_ID_CTRL + 1;

trait GenericNetlinkProtocolServer<S>: Send {
    /// Return the unique name for this generic netlink protocol.
    ///
    /// This name is used by the ctrl server to identify this server for clients.
    fn name(&self) -> String;

    /// Return the multicast groups that are supported by this protocol family.
    ///
    /// Each multicast group is assigned a unique ID by the ctrl server.
    fn multicast_groups(&self) -> Vec<String> {
        vec![]
    }

    /// Handle a netlink message targeted to this server.
    ///
    /// The given payload contains the generic netlink header and all subsequent data.
    /// Protocol servers should implement their own generic netlink families and
    /// deserialize messages using `GenlMessage::<_>::deserialize`.
    fn handle_message(&self, netlink_header: NetlinkHeader, payload: Vec<u8>, sender: &mut S);
}

fn extract_family_names(genl_ctrl: GenlCtrl) -> Vec<String> {
    genl_ctrl
        .nlas
        .into_iter()
        .filter_map(
            |attr| {
                if let GenlCtrlAttrs::FamilyName(name) = attr {
                    Some(name)
                } else {
                    None
                }
            },
        )
        .collect()
}

/// All state required to the generic netlink server. This struct assumes
/// synchronous access, and should be kept inside of a GenericNetlinkServer.
struct GenericNetlinkServerState<S> {
    /// Mapping from generic family server name to its assigned ID value.
    server_ids: HashMap<String, u16>,
    /// Servers for specific generic netlink families. Servers are stored in this
    /// list by order of ID, such that protocol N is in servers[N - MIN_FAMILY_ID].
    servers: Vec<Box<dyn GenericNetlinkProtocolServer<S>>>,
    /// Multicast groups, identified by (family name, group name). Multicast
    /// group IDs are assigned uniquely across all generic families.
    multicast_groups: HashMap<(String, String), u32>,
    /// Counter used to generate unique ID values for all multicast groups.
    multicast_group_id_counter: u32,
}

impl<S: Sender<GenericMessage>> GenericNetlinkServerState<S> {
    fn new() -> Self {
        let mut state = Self {
            server_ids: HashMap::new(),
            servers: vec![],
            multicast_groups: HashMap::new(),
            multicast_group_id_counter: 0,
        };
        // TODO(fxbug.dev/128857): Add dynamic family support. Right now this is only
        // structured to support static family additions.
        state.add_server(Box::new(nl80211::Nl80211ProtocolServer {}));
        state
    }

    fn add_server(&mut self, server: Box<dyn GenericNetlinkProtocolServer<S>>) {
        match (self.servers.len() as u16).checked_add(MIN_FAMILY_ID) {
            Some(new_family_id) => {
                self.server_ids.insert(server.name(), new_family_id);
                self.servers.push(server);
            }
            None => {
                log_error!(
                    tag = NETLINK_LOG_TAG,
                    "Failed to add generic netlink server: too many servers"
                );
            }
        }
    }

    fn get_server(&self, family_id: u16) -> Option<&dyn GenericNetlinkProtocolServer<S>> {
        if family_id >= MIN_FAMILY_ID && ((family_id - MIN_FAMILY_ID) as usize) < self.servers.len()
        {
            Some(&*self.servers[(family_id - MIN_FAMILY_ID) as usize])
        } else {
            None
        }
    }

    fn get_multicast_group_id(&mut self, family: String, group: String) -> u32 {
        *self.multicast_groups.entry((family, group)).or_insert_with(|| {
            self.multicast_group_id_counter += 1;
            self.multicast_group_id_counter
        })
    }

    fn handle_ctrl_message(
        &mut self,
        netlink_header: NetlinkHeader,
        genl_message: GenlMessage<GenlCtrl>,
        sender: &mut S,
    ) {
        let (genl_header, genl_ctrl) = genl_message.into_parts();
        match genl_ctrl.cmd {
            GenlCtrlCmd::GetFamily => {
                let family_names = extract_family_names(genl_ctrl);
                log_info!(tag = NETLINK_LOG_TAG, "Netlink GetFamily request: {:?}", family_names);

                for family in &family_names {
                    if let Some(id) = self.server_ids.get(family) {
                        log_info!(
                            tag = NETLINK_LOG_TAG,
                            "Serving requested netlink family {}",
                            family
                        );
                        let resp_ctrl = GenlCtrl {
                            cmd: GenlCtrlCmd::NewFamily,
                            nlas: vec![
                                GenlCtrlAttrs::FamilyId(*id),
                                GenlCtrlAttrs::FamilyName(family.to_string()),
                                GenlCtrlAttrs::McastGroups(
                                    self.get_server(*id)
                                        .expect("Known server ID should always exist")
                                        .multicast_groups()
                                        .into_iter()
                                        .map(|name| {
                                            vec![
                                                McastGrpAttrs::Name(name.clone()),
                                                McastGrpAttrs::Id(self.get_multicast_group_id(
                                                    family.to_string(),
                                                    name,
                                                )),
                                            ]
                                        })
                                        .collect(),
                                ),
                            ],
                        };
                        let mut genl_message = GenlMessage::from_parts(genl_header, resp_ctrl);
                        genl_message.finalize();
                        let mut message = NetlinkMessage::new(
                            netlink_header,
                            NetlinkPayload::InnerMessage(GenericMessage::Ctrl(genl_message)),
                        );
                        message.finalize();
                        sender.send(message, None);
                    } else {
                        log_warn!(
                            tag = NETLINK_LOG_TAG,
                            "Cannot serve requested netlink family {}",
                            family
                        );
                    }
                }
            }
            // TODO(fxbug.dev/128857): Support additionl Ctrl commands.
            other => log_error!(tag = NETLINK_LOG_TAG, "Unsupported GenlCtrlCmd: {:?}", other),
        }
    }

    fn handle_generic_message(&mut self, message: NetlinkMessage<GenericMessage>, sender: &mut S) {
        let (netlink_header, payload) = message.into_parts();
        let req = match payload {
            NetlinkPayload::InnerMessage(p) => p,
            p => {
                log_error!(tag = NETLINK_LOG_TAG, "Dropping unexpected netlink payload: {:?}", p);
                return;
            }
        };
        match req {
            GenericMessage::Ctrl(ctrl_message) => {
                self.handle_ctrl_message(netlink_header, ctrl_message, sender)
            }
            GenericMessage::Other { family, payload } => match self.get_server(family) {
                Some(server) => server.handle_message(netlink_header, payload, sender),
                None => log_info!(
                    tag = NETLINK_LOG_TAG,
                    "Ignoring generic netlink message with unsupported family {}",
                    family
                ),
            },
        }
    }
}

/// Coordinates all generic netlink clients and servers.
struct GenericNetlinkServer<S> {
    state: Mutex<GenericNetlinkServerState<S>>,
    _data: std::marker::PhantomData<S>,
}

impl<S: Sender<GenericMessage> + Send> GenericNetlinkServer<S> {
    fn new() -> Self {
        Self { state: Mutex::new(GenericNetlinkServerState::new()), _data: Default::default() }
    }

    async fn run_generic_netlink_client(&self, mut client: GenericNetlinkClient<S>) {
        log_info!(tag = NETLINK_LOG_TAG, "Registered new generic netlink client");
        loop {
            match client.receiver.next().await {
                Some(message) => {
                    self.state.lock().unwrap().handle_generic_message(message, &mut client.sender)
                }
                None => {
                    log_info!(tag = NETLINK_LOG_TAG, "Generic netlink client exited");
                    return;
                }
            }
        }
    }
}

pub(crate) struct GenericNetlinkClient<S> {
    sender: S,
    receiver: mpsc::UnboundedReceiver<NetlinkMessage<GenericMessage>>,
}

pub(crate) struct GenericNetlink<S> {
    new_client_sender: mpsc::UnboundedSender<GenericNetlinkClient<S>>,
}

async fn run_generic_netlink<S: Sender<GenericMessage> + Send>(
    new_client_receiver: mpsc::UnboundedReceiver<GenericNetlinkClient<S>>,
) {
    let server = GenericNetlinkServer::new();
    new_client_receiver
        .for_each_concurrent(None, |client| server.run_generic_netlink_client(client))
        .await;
}

impl<S: Sender<GenericMessage> + Send> GenericNetlink<S> {
    pub fn new() -> (Self, impl Future<Output = ()> + Send) {
        let (new_client_sender, new_client_receiver) = mpsc::unbounded();
        let generic_netlink = Self { new_client_sender };
        let netlink_fut = run_generic_netlink(new_client_receiver);
        (generic_netlink, netlink_fut)
    }

    pub fn new_generic_client(
        &self,
        sender: S,
        receiver: mpsc::UnboundedReceiver<NetlinkMessage<GenericMessage>>,
    ) -> Result<(), anyhow::Error> {
        let new_client = GenericNetlinkClient { sender, receiver };
        self.new_client_sender.unbounded_send(new_client).map_err(|e| e.into())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_async::TestExecutor,
        netlink::multicast_groups::ModernGroup,
        netlink_packet_core::{NetlinkPayload, NetlinkSerializable},
        netlink_packet_generic::GenlHeader,
        std::sync::Arc,
        std::task::Poll,
    };

    #[derive(Clone)]
    struct TestSender<M> {
        messages: Arc<Mutex<Vec<NetlinkMessage<M>>>>,
    }

    impl<M: Clone + NetlinkSerializable + Send + Sync + 'static> Sender<M> for TestSender<M> {
        fn send(&mut self, message: NetlinkMessage<M>, _group: Option<ModernGroup>) {
            self.messages.lock().unwrap().push(message);
        }
    }

    const TEST_FAMILY: &str = "test_family";
    const MCAST_GROUP_1: &str = "m1";
    const MCAST_GROUP_2: &str = "m2";

    fn getfamily_request() -> NetlinkMessage<GenericMessage> {
        let getfamily_ctrl = GenlCtrl {
            cmd: GenlCtrlCmd::GetFamily,
            nlas: vec![GenlCtrlAttrs::FamilyName(TEST_FAMILY.to_string())],
        };
        let mut genl_message =
            GenlMessage::new(GenlHeader { cmd: 0, version: 0 }, getfamily_ctrl, GENL_ID_CTRL);
        genl_message.finalize();
        let mut netlink_message = NetlinkMessage::new(
            Default::default(),
            NetlinkPayload::InnerMessage(GenericMessage::Ctrl(genl_message)),
        );
        netlink_message.finalize();
        netlink_message
    }

    struct TestFamily {
        messages_to_server: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl<S> GenericNetlinkProtocolServer<S> for TestFamily {
        fn name(&self) -> String {
            TEST_FAMILY.into()
        }

        fn multicast_groups(&self) -> Vec<String> {
            vec![MCAST_GROUP_1.to_string(), MCAST_GROUP_2.to_string()]
        }

        fn handle_message(
            &self,
            _netlink_header: NetlinkHeader,
            payload: Vec<u8>,
            _sender: &mut S,
        ) {
            self.messages_to_server.lock().unwrap().push(payload);
        }
    }

    #[test]
    fn test_ctrl_getfamily_missing() {
        let mut exec = TestExecutor::new();
        let server = GenericNetlinkServer::<TestSender<GenericMessage>>::new();
        let messages = Arc::new(Mutex::new(vec![]));
        let (sender, receiver) = mpsc::unbounded();
        let client =
            GenericNetlinkClient { sender: TestSender { messages: messages.clone() }, receiver };
        let mut client_fut = Box::pin(server.run_generic_netlink_client(client));
        sender.unbounded_send(getfamily_request()).expect("Failed to send getfamily request");
        assert!(exec.run_until_stalled(&mut client_fut) == Poll::Pending);

        // The family doesn't exist, so no response should be sent.
        assert!(messages.lock().unwrap().is_empty());
    }

    #[test]
    fn test_ctrl_getfamily() {
        let mut exec = TestExecutor::new();
        let server = GenericNetlinkServer::<TestSender<GenericMessage>>::new();
        let messages_to_server = Arc::new(Mutex::new(vec![]));
        server.state.lock().unwrap().add_server(Box::new(TestFamily { messages_to_server }));
        let messages_to_client = Arc::new(Mutex::new(vec![]));
        let (sender, receiver) = mpsc::unbounded();
        let client = GenericNetlinkClient {
            sender: TestSender { messages: messages_to_client.clone() },
            receiver,
        };
        let mut client_fut = Box::pin(server.run_generic_netlink_client(client));
        sender.unbounded_send(getfamily_request()).expect("Failed to send getfamily request");
        assert!(exec.run_until_stalled(&mut client_fut) == Poll::Pending);

        // Verify that we got all expected information in the response.
        assert!(messages_to_client.lock().unwrap().len() == 1);
        let (_netlink_header, payload) =
            messages_to_client.lock().unwrap().pop().unwrap().into_parts();
        let (_genl_header, ctrl_payload) = assert_matches!(
            payload,
            NetlinkPayload::InnerMessage(GenericMessage::Ctrl(m)) => m.into_parts());
        assert_eq!(ctrl_payload.cmd, GenlCtrlCmd::NewFamily);
        assert!(ctrl_payload
            .nlas
            .iter()
            .any(|nla| *nla == GenlCtrlAttrs::FamilyName(TEST_FAMILY.into())));
        assert!(ctrl_payload.nlas.iter().any(|nla| matches!(nla, GenlCtrlAttrs::FamilyId(_))));
        let multicast_groups = ctrl_payload
            .nlas
            .iter()
            .filter_map(
                |nla| if let GenlCtrlAttrs::McastGroups(vec) = nla { Some(vec) } else { None },
            )
            .next()
            .expect("No multicast groups");
        assert_eq!(multicast_groups.len(), 2);
        assert!(multicast_groups.iter().any(|group| {
            group.iter().any(|attr| matches!(attr, McastGrpAttrs::Id(_)));
            group.iter().any(|attr| *attr == McastGrpAttrs::Name(MCAST_GROUP_1.into()))
        }));
        assert!(multicast_groups.iter().any(|group| {
            group.iter().any(|attr| matches!(attr, McastGrpAttrs::Id(_)));
            group.iter().any(|attr| *attr == McastGrpAttrs::Name(MCAST_GROUP_2.into()))
        }));
    }

    #[test]
    fn test_send_family_message() {
        let mut exec = TestExecutor::new();
        let server = GenericNetlinkServer::<TestSender<GenericMessage>>::new();
        let messages_to_server = Arc::new(Mutex::new(vec![]));
        server
            .state
            .lock()
            .unwrap()
            .add_server(Box::new(TestFamily { messages_to_server: messages_to_server.clone() }));
        let messages_to_client = Arc::new(Mutex::new(vec![]));
        let (sender, receiver) = mpsc::unbounded();
        let client = GenericNetlinkClient {
            sender: TestSender { messages: messages_to_client.clone() },
            receiver,
        };
        let mut client_fut = Box::pin(server.run_generic_netlink_client(client));
        sender.unbounded_send(getfamily_request()).expect("Failed to send getfamily request");
        assert!(exec.run_until_stalled(&mut client_fut) == Poll::Pending);

        assert!(messages_to_client.lock().unwrap().len() == 1);
        let (_netlink_header, payload) =
            messages_to_client.lock().unwrap().pop().unwrap().into_parts();
        let (_genl_header, ctrl_payload) = assert_matches!(
            payload,
            NetlinkPayload::InnerMessage(GenericMessage::Ctrl(m)) => m.into_parts());
        let family_id = *ctrl_payload
            .nlas
            .iter()
            .filter_map(|nla| if let GenlCtrlAttrs::FamilyId(id) = nla { Some(id) } else { None })
            .next()
            .expect("Could not find family id");

        let mut netlink_message = NetlinkMessage::new(
            Default::default(),
            NetlinkPayload::InnerMessage(GenericMessage::Other {
                family: family_id,
                payload: vec![0, 1, 2, 3],
            }),
        );
        netlink_message.finalize();
        sender.unbounded_send(netlink_message).expect("Failed to send test family message");

        assert!(exec.run_until_stalled(&mut client_fut) == Poll::Pending);
        assert_eq!(messages_to_server.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_send_invalid_family_message() {
        let mut exec = TestExecutor::new();
        let server = GenericNetlinkServer::<TestSender<GenericMessage>>::new();
        let messages_to_server = Arc::new(Mutex::new(vec![]));
        server
            .state
            .lock()
            .unwrap()
            .add_server(Box::new(TestFamily { messages_to_server: messages_to_server.clone() }));
        let messages_to_client = Arc::new(Mutex::new(vec![]));
        let (sender, receiver) = mpsc::unbounded();
        let client = GenericNetlinkClient {
            sender: TestSender { messages: messages_to_client.clone() },
            receiver,
        };
        let mut client_fut = Box::pin(server.run_generic_netlink_client(client));

        let mut netlink_message = NetlinkMessage::new(
            Default::default(),
            NetlinkPayload::InnerMessage(GenericMessage::Other {
                family: 1337,
                payload: vec![0, 1, 2, 3],
            }),
        );
        netlink_message.finalize();
        sender.unbounded_send(netlink_message).expect("Failed to send test family message");
        assert!(exec.run_until_stalled(&mut client_fut) == Poll::Pending);
        assert!(messages_to_server.lock().unwrap().is_empty());
        assert!(messages_to_client.lock().unwrap().is_empty());
    }
}
