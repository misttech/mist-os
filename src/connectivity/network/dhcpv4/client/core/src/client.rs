// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the DHCP client state machine.

use crate::deps::{self, DatagramInfo, Instant as _};
use anyhow::Context as _;
use dhcp_protocol::{AtLeast, AtMostBytes};
use futures::{
    channel::mpsc, pin_mut, select, FutureExt as _, Stream, StreamExt as _, TryStreamExt as _,
};
use rand::Rng as _;
use std::{net::Ipv4Addr, num::NonZeroU32};

pub const SERVER_PORT: std::num::NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT);

pub const CLIENT_PORT: std::num::NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::CLIENT_PORT);

/// Unexpected, non-recoverable errors encountered by the DHCP client.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("error while using socket: {0:?}")]
    Socket(deps::SocketError),
}

/// The reason the DHCP client exited.
pub enum ExitReason {
    GracefulShutdown,
}

/// All possible core state machine states.
pub enum State<I> {
    Init(Init),
    Selecting(Selecting<I>),
    Requesting(Requesting<I>),
}

/// The next step to take after running the core state machine for one step.
pub enum Step<I> {
    NextState(State<I>),
    Exit(ExitReason),
}

impl<I: deps::Instant> State<I> {
    pub async fn run<C: deps::Clock<Instant = I>>(
        &self,
        config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        clock: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<Step<I>, Error> {
        match self {
            State::Init(init) => Ok(Step::NextState(State::Selecting(init.do_init(rng, clock)))),
            State::Selecting(selecting) => match selecting
                .do_selecting(config, packet_socket_provider, rng, clock, stop_receiver)
                .await?
            {
                SelectingOutcome::GracefulShutdown => Ok(Step::Exit(ExitReason::GracefulShutdown)),
                SelectingOutcome::Requesting(requesting) => {
                    Ok(Step::NextState(State::Requesting(requesting)))
                }
            },
            State::Requesting(_requesting) => {
                todo!("TODO(https://fxbug.dev/123609): implement requesting state");
            }
        }
    }
}

impl<I> Default for State<I> {
    fn default() -> Self {
        State::Init(Init::default())
    }
}

/// Configuration for the DHCP client to be used while negotiating with DHCP
/// servers.
#[derive(Clone)]
pub struct ClientConfig {
    /// The hardware address of the interface on which the DHCP client is run.
    pub client_hardware_address: net_types::ethernet::Mac,
    /// If set, a unique-on-the-local-network string to be used to identify this
    /// device while negotiating with DHCP servers.
    pub client_identifier:
        Option<AtLeast<2, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<u8>>>>,
    /// A list of parameters to request from DHCP servers.
    pub parameter_request_list: Option<
        AtLeast<1, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<dhcp_protocol::OptionCode>>>,
    >,
    /// If set, the preferred IP address lease time in seconds.
    pub preferred_lease_time_secs: Option<NonZeroU32>,
    /// If set, the IP address to request from DHCP servers.
    pub requested_ip_address: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq)]
struct DiscoverOptions {
    xid: TransactionId,
}

/// Transaction ID for an exchange of DHCP messages.
///
/// Per [RFC 2131], "Transaction ID, a random number chosen by the client, used
/// by the client and server to associate messages and responses between a
/// client and a server."
///
/// [RFC 2131]: https://www.rfc-editor.org/rfc/inline-errata/rfc2131.html#section-4.3.1
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransactionId(
    // While the DHCP RFC does not require that the XID be nonzero, it's helpful
    // to maintain that it is nonzero in order to make it clear that it is set
    // while debugging.
    NonZeroU32,
);

/// The initial state as depicted in the state-transition diagram in [RFC 2131].
/// [RFC 2131]: https://www.rfc-editor.org/rfc/inline-errata/rfc2131.html#section-4.4
#[derive(Default)]
pub struct Init {}

impl Init {
    /// Generates a random transaction ID, records the starting time, and
    /// transitions to Selecting.
    pub fn do_init<C: deps::Clock>(
        &self,
        rng: &mut impl deps::RngProvider,
        clock: &C,
    ) -> Selecting<C::Instant> {
        let discover_options = DiscoverOptions {
            xid: TransactionId(NonZeroU32::new(rng.get_rng().gen_range(1..=u32::MAX)).unwrap()),
        };
        Selecting {
            discover_options,
            // Per RFC 2131 section 4.4.1, "The client records its own local time
            // for later use in computing the lease expiration" when it starts
            // sending DHCPDISCOVERs.
            start_time: clock.now(),
        }
    }
}

async fn send_with_retransmits<T: Clone + Send>(
    time: &impl deps::Clock,
    retransmit_schedule: impl Iterator<Item = std::time::Duration>,
    message: &[u8],
    socket: &impl deps::Socket<T>,
    dest: T,
) -> Result<(), Error> {
    for wait_duration in std::iter::once(None).chain(retransmit_schedule.map(Some)) {
        if let Some(wait_duration) = wait_duration {
            time.wait_until(time.now().add(wait_duration)).await;
        }
        // TODO(https://fxbug.dev/124593): better indicate recoverable
        // vs. non-recoverable error types.
        socket.send_to(message, dest.clone()).await.map_err(Error::Socket)?;
    }
    Ok(())
}

fn default_retransmit_schedule(
    rng: &mut (impl rand::Rng + ?Sized),
) -> impl Iterator<Item = std::time::Duration> + '_ {
    const MILLISECONDS_PER_SECOND: i32 = 1000;
    [4i32, 8, 16, 32]
        .into_iter()
        .chain(std::iter::repeat(64))
        // Per RFC 2131 Section 4.3.1, "the delay before the first
        // retransmission SHOULD be 4 seconds randomized by the value of a
        // uniform random number chosen from the range -1 to +1.  [...] The
        // delay before the next retransmission SHOULD be 8 seconds randomized
        // by the value of a uniform number chosen from the range -1 to +1.  The
        // retransmission delay SHOULD be doubled with subsequent
        // retransmissions up to a maximum of 64 seconds."
        .zip(std::iter::from_fn(|| {
            Some(rng.gen_range((-MILLISECONDS_PER_SECOND)..=MILLISECONDS_PER_SECOND))
        }))
        .map(|(base_seconds, jitter_millis)| {
            let millis = u64::try_from(base_seconds * MILLISECONDS_PER_SECOND + jitter_millis)
                .expect("retransmit wait is never negative");
            std::time::Duration::from_millis(millis)
        })
}

// This is assumed to be an appropriate buffer size due to Ethernet's common MTU
// of 1500 bytes.
const BUFFER_SIZE: usize = 1500;

fn recv_stream<'a, T, U: Send>(
    socket: &'a impl deps::Socket<U>,
    recv_buf: &'a mut [u8],
    parser: impl Fn(&[u8], U) -> T + 'a,
) -> impl Stream<Item = Result<T, Error>> + 'a {
    futures::stream::try_unfold((recv_buf, parser), move |(recv_buf, parser)| async move {
        // TODO(https://fxbug.dev/124593): better indicate recoverable
        // vs. non-recoverable error types.
        let DatagramInfo { length, address } =
            socket.recv_from(recv_buf).await.map_err(Error::Socket)?;
        let raw_msg = &recv_buf[..length];
        let parsed = parser(raw_msg, address);
        Ok(Some((parsed, (recv_buf, parser))))
    })
}

fn build_discover(
    ClientConfig {
        client_hardware_address,
        client_identifier,
        parameter_request_list,
        preferred_lease_time_secs,
        requested_ip_address,
    }: &ClientConfig,
    DiscoverOptions { xid: TransactionId(xid) }: &DiscoverOptions,
) -> dhcp_protocol::Message {
    use dhcp_protocol::DhcpOption;

    // The following fields are set according to
    // https://www.rfc-editor.org/rfc/rfc2131#section-4.4.1.
    dhcp_protocol::Message {
        op: dhcp_protocol::OpCode::BOOTREQUEST,
        xid: u32::from(*xid),
        // Must be 0, or the number of seconds since the DHCP process started.
        // Since it has to be the same as in DHCPREQUEST, it's easiest to have it be 0.
        secs: 0,
        // Because packet sockets are available to us, the DHCP client is
        // assumed to be able to receive unicast datagrams without having an IP
        // address configured yet.
        bdcast_flag: false,
        ciaddr: Ipv4Addr::UNSPECIFIED,
        yiaddr: Ipv4Addr::UNSPECIFIED,
        siaddr: Ipv4Addr::UNSPECIFIED,
        giaddr: Ipv4Addr::UNSPECIFIED,
        chaddr: *client_hardware_address,
        sname: String::new(),
        file: String::new(),
        options: [
            preferred_lease_time_secs.map(|n| DhcpOption::IpAddressLeaseTime(n.into())),
            requested_ip_address.map(DhcpOption::RequestedIpAddress),
            // TODO(https://fxbug.dev/122602): Avoid cloning
            parameter_request_list.clone().map(DhcpOption::ParameterRequestList),
            client_identifier.clone().map(DhcpOption::ClientIdentifier),
        ]
        .into_iter()
        .flatten()
        .chain([DhcpOption::DhcpMessageType(dhcp_protocol::MessageType::DHCPDISCOVER)])
        .collect(),
    }
}

#[derive(Debug)]
pub enum SelectingOutcome<I> {
    GracefulShutdown,
    Requesting(Requesting<I>),
}

/// The Selecting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://www.rfc-editor.org/rfc/inline-errata/rfc2131.html#section-4.4
pub struct Selecting<I> {
    discover_options: DiscoverOptions,
    // The time at which the DHCP transaction was initiated (used as the offset
    // from which lease expiration times are computed).
    start_time: I,
}

impl<I: deps::Instant> Selecting<I> {
    /// Executes the Selecting state.
    ///
    /// Transmits (and retransmits, if necessary) DHCPDISCOVER messages, and
    /// receives DHCPOFFER messages, on a packet socket. Tries to select a
    /// DHCPOFFER. If successful, transitions to Requesting.
    pub async fn do_selecting<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<SelectingOutcome<I>, Error> {
        // TODO(https://fxbug.dev/124724): avoid dropping/recreating the packet
        // socket unnecessarily by taking an `&impl
        // deps::Socket<net_types::ethernet::Mac>` here instead.
        let socket = packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;
        let Selecting { discover_options, start_time } = self;
        let message = build_discover(client_config, discover_options);

        let message = crate::parse::serialize_dhcp_message_to_ip_packet(
            message,
            Ipv4Addr::UNSPECIFIED, // src_ip
            CLIENT_PORT,
            Ipv4Addr::BROADCAST, // dst_ip
            SERVER_PORT,
        );

        let send_fut = send_with_retransmits(
            time,
            default_retransmit_schedule(rng.get_rng()),
            message.as_ref(),
            &socket,
            /* dest= */ net_types::ethernet::Mac::BROADCAST,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];
        let offer_fields_stream = recv_stream(&socket, &mut recv_buf, |packet, src_addr| {
            // We don't care about the src addr of incoming offers, because we
            // identify DHCP servers via the Server Identifier option.
            let _: net_types::ethernet::Mac = src_addr;
            let message = crate::parse::parse_dhcp_message_from_ip_packet(packet, CLIENT_PORT)
                .context("error while parsing DHCP message from IP packet")?;
            validate_message(discover_options, client_config, &message)
                .context("invalid DHCP message")?;
            crate::parse::fields_to_retain_from_selecting(message)
                .context("error while retrieving fields to use in DHCPREQUEST from DHCP message")
        })
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(fields) => Some(fields),
                Err(error) => {
                    tracing::warn!("discarding incoming packet: {}", error);
                    None
                }
            })
        })
        .fuse();

        pin_mut!(send_fut, offer_fields_stream);

        select! {
            send_discovers_result = send_fut => {
                send_discovers_result?;
                unreachable!("should never stop retransmitting DHCPDISCOVER unless we hit an error");
            },
            () = stop_receiver.select_next_some() => {
                Ok(SelectingOutcome::GracefulShutdown)
            },
            fields_to_use_in_request_result = offer_fields_stream.select_next_some() => {
                let fields_from_offer_to_use_in_request = fields_to_use_in_request_result?;

                // Currently, we take the naive approach of accepting the first
                // DHCPOFFER we see without doing any special selection logic.
                Ok(SelectingOutcome::Requesting(Requesting {
                    discover_options: discover_options.clone(),
                    fields_from_offer_to_use_in_request,
                    start_time: *start_time,
                }))
            }
        }
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
enum ValidateMessageError {
    #[error("xid {actual} doesn't match expected xid {expected}")]
    WrongXid { expected: u32, actual: u32 },
    #[error("chaddr {actual} doesn't match expected chaddr {expected}")]
    WrongChaddr { expected: net_types::ethernet::Mac, actual: net_types::ethernet::Mac },
}

fn validate_message(
    DiscoverOptions { xid: TransactionId(my_xid) }: &DiscoverOptions,
    ClientConfig {
        client_hardware_address: my_chaddr,
        client_identifier: _,
        parameter_request_list: _,
        preferred_lease_time_secs: _,
        requested_ip_address: _,
    }: &ClientConfig,
    dhcp_protocol::Message {
        op: _,
        xid: msg_xid,
        secs: _,
        bdcast_flag: _,
        ciaddr: _,
        yiaddr: _,
        siaddr: _,
        giaddr: _,
        chaddr: msg_chaddr,
        sname: _,
        file: _,
        options: _,
    }: &dhcp_protocol::Message,
) -> Result<(), ValidateMessageError> {
    if *msg_xid != u32::from(*my_xid) {
        return Err(ValidateMessageError::WrongXid { expected: my_xid.get(), actual: *msg_xid });
    }

    if msg_chaddr != my_chaddr {
        return Err(ValidateMessageError::WrongChaddr {
            expected: *my_chaddr,
            actual: *msg_chaddr,
        });
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct Requesting<I> {
    discover_options: DiscoverOptions,
    fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest,
    start_time: I,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::deps::testutil::{
        advance, run_until_next_timers_fire, FakeRngProvider, FakeSocket, FakeSocketProvider,
        FakeTimeController,
    };
    use crate::deps::{Clock as _, DatagramInfo, Socket as _};
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use futures::join;
    use net_declare::{net_mac, std_ip_v4};
    use test_case::test_case;

    fn initialize_logging() {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::INFO)
            .finish();

        // Intentionally don't use the result here, since it'll succeed with the
        // first test case that calls it and fail with the others.
        let _: Result<_, _> = tracing::subscriber::set_global_default(subscriber);
    }

    const TEST_MAC_ADDRESS: net_types::ethernet::Mac = net_mac!("01:01:01:01:01:01");
    const TEST_SERVER_MAC_ADDRESS: net_types::ethernet::Mac = net_mac!("02:02:02:02:02:02");
    const OTHER_MAC_ADDRESS: net_types::ethernet::Mac = net_mac!("03:03:03:03:03:03");

    const TEST_PARAMETER_REQUEST_LIST: [dhcp_protocol::OptionCode; 3] = [
        dhcp_protocol::OptionCode::SubnetMask,
        dhcp_protocol::OptionCode::Router,
        dhcp_protocol::OptionCode::DomainNameServer,
    ];

    const SERVER_IP: Ipv4Addr = std_ip_v4!("192.168.1.1");
    const YIADDR: Ipv4Addr = std_ip_v4!("198.168.1.5");
    const OTHER_ADDR: Ipv4Addr = std_ip_v4!("198.168.1.6");
    const DEFAULT_LEASE_LENGTH_SECONDS: u32 = 100;
    const MAX_LEASE_LENGTH_SECONDS: u32 = 200;

    fn test_client_config() -> ClientConfig {
        ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            parameter_request_list: Some(TEST_PARAMETER_REQUEST_LIST.into()),
            preferred_lease_time_secs: None,
            requested_ip_address: None,
        }
    }

    #[test]
    fn do_init_uses_rng() {
        let mut rng = FakeRngProvider::new(0);
        let time = FakeTimeController::new();
        let arbitrary_start_time = std::time::Duration::from_secs(42);
        advance(&time, arbitrary_start_time);

        let Selecting {
            discover_options: DiscoverOptions { xid: xid_a },
            start_time: start_time_a,
        } = Init {}.do_init(&mut rng, &time);
        let Selecting {
            discover_options: DiscoverOptions { xid: xid_b },
            start_time: start_time_b,
        } = Init {}.do_init(&mut rng, &time);
        assert_ne!(xid_a, xid_b);
        assert_eq!(start_time_a, arbitrary_start_time);
        assert_eq!(start_time_b, arbitrary_start_time);
    }

    #[test]
    fn do_selecting_obeys_graceful_shutdown() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let selecting = Selecting {
            discover_options: DiscoverOptions { xid: TransactionId(nonzero_ext::nonzero!(1u32)) },
            start_time: std::time::Duration::from_secs(0),
        };
        let mut rng = FakeRngProvider::new(0);

        let (_server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (stop_sender, mut stop_receiver) = mpsc::unbounded();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        let wait_fut = async {
            // Wait some arbitrary amount of time to ensure `do_selecting` is waiting on a reply.
            // Note that this is fake time, not 30 actual seconds.
            time.wait_until(std::time::Duration::from_secs(30)).await;
        }
        .fuse();

        pin_mut!(selecting_fut, wait_fut);

        let main_future = async {
            select! {
                _ = selecting_fut => unreachable!("should keep retransmitting DHCPDISCOVER forever"),
                () = wait_fut => (),
            }
        };
        pin_mut!(main_future);

        loop {
            match run_until_next_timers_fire(&mut executor, time, &mut main_future) {
                std::task::Poll::Ready(()) => break,
                std::task::Poll::Pending => (),
            }
        }

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let selecting_result = selecting_fut.now_or_never().expect(
            "selecting_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(selecting_result, Ok(SelectingOutcome::GracefulShutdown));
    }

    #[test]
    fn do_selecting_sends_discover() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let selecting = Selecting {
            discover_options: DiscoverOptions { xid: TransactionId(nonzero_ext::nonzero!(1u32)) },
            start_time: std::time::Duration::from_secs(0),
        };
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        const EXPECTED_RANGES: [(u64, u64); 7] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33), (63, 65), (63, 65)];

        let receive_fut = async_stream::stream! {
            loop {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } =
                    server_end.recv_from(&mut recv_buf)
                        .await
                        .expect("recv_from on test socket should succeed");

                assert_eq!(address, net_types::ethernet::Mac::BROADCAST);

                let msg = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT),
                )
                .expect("received packet should parse as DHCP message");

                assert_eq!(
                    msg,
                    dhcp_protocol::Message {
                        op: dhcp_protocol::OpCode::BOOTREQUEST,
                        xid: msg.xid,
                        secs: 0,
                        bdcast_flag: false,
                        ciaddr: Ipv4Addr::UNSPECIFIED,
                        yiaddr: Ipv4Addr::UNSPECIFIED,
                        siaddr: Ipv4Addr::UNSPECIFIED,
                        giaddr: Ipv4Addr::UNSPECIFIED,
                        chaddr: TEST_MAC_ADDRESS,
                        sname: String::new(),
                        file: String::new(),
                        options: vec![
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                TEST_PARAMETER_REQUEST_LIST.into()
                            ),
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPDISCOVER
                            ),
                        ],
                    }
                );
                yield time.now();
            }
        }
        .take(EXPECTED_RANGES.len())
        .collect::<Vec<_>>()
        .fuse();

        pin_mut!(selecting_fut, receive_fut);

        let main_future = async {
            select! {
                _ = selecting_fut => unreachable!("should keep retransmitting DHCPDISCOVER forever"),
                received = receive_fut => received,
            }
        };
        pin_mut!(main_future);

        let received = loop {
            match run_until_next_timers_fire(&mut executor, time, &mut main_future) {
                std::task::Poll::Ready(received) => break received,
                std::task::Poll::Pending => (),
            }
        };

        let mut previous_time = std::time::Duration::from_secs(0);
        for ((start, end), received_time) in EXPECTED_RANGES.into_iter().zip(received) {
            let duration_range =
                std::time::Duration::from_secs(start)..=std::time::Duration::from_secs(end);
            assert!(duration_range.contains(&(received_time - previous_time)));
            previous_time = received_time;
        }
    }

    const XID: NonZeroU32 = nonzero_ext::nonzero!(1u32);
    #[test_case(u32::from(XID), TEST_MAC_ADDRESS => Ok(()) ; "accepts good reply")]
    #[test_case(u32::from(XID), TEST_SERVER_MAC_ADDRESS => Err(
        ValidateMessageError::WrongChaddr {
            expected: TEST_MAC_ADDRESS,
            actual: TEST_SERVER_MAC_ADDRESS,
        }) ; "rejects wrong chaddr")]
    #[test_case(u32::from(XID).wrapping_add(1), TEST_MAC_ADDRESS => Err(
        ValidateMessageError::WrongXid {
            expected: u32::from(XID),
            actual: u32::from(XID).wrapping_add(1),
        }) ; "rejects wrong xid")]
    fn test_validate_message(
        message_xid: u32,
        message_chaddr: net_types::ethernet::Mac,
    ) -> Result<(), ValidateMessageError> {
        let discover_options = DiscoverOptions { xid: TransactionId(XID) };
        let client_config = ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            parameter_request_list: Some(TEST_PARAMETER_REQUEST_LIST.into()),
            preferred_lease_time_secs: None,
            requested_ip_address: None,
        };

        let reply = dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREPLY,
            xid: message_xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: message_chaddr,
            sname: String::new(),
            file: String::new(),
            options: Vec::new(),
        };

        validate_message(&discover_options, &client_config, &reply)
    }

    #[allow(clippy::unused_unit)]
    #[test_case(false ; "with no garbage traffic on link")]
    #[test_case(true ; "ignoring garbage replies to discover")]
    fn do_selecting_good_offer(reply_to_discover_with_garbage: bool) {
        initialize_logging();

        let mut rng = FakeRngProvider::new(0);
        let time = FakeTimeController::new();

        let arbitrary_start_time = std::time::Duration::from_secs(42);
        advance(&time, arbitrary_start_time);

        let selecting = Init {}.do_init(&mut rng, &time);
        let TransactionId(xid) = selecting.discover_options.xid;

        let (server_end, client_end) = FakeSocket::<net_types::ethernet::Mac>::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let client_config = test_client_config();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            if reply_to_discover_with_garbage {
                let DatagramInfo { length: _, address: dst_addr } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");
                assert_eq!(dst_addr, net_types::ethernet::Mac::BROADCAST);

                server_end
                    .send_to(b"hello", OTHER_MAC_ADDRESS)
                    .await
                    .expect("send_to with garbage data should succeed");
            }

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, net_types::ethernet::Mac::BROADCAST);

            // `dhcp_protocol::Message` intentionally doesn't implement `Clone`,
            // so we re-parse instead for testing purposes.
            let parse_msg = || {
                crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT),
                )
                .expect("received packet on test socket should parse as DHCP message")
            };

            let msg = parse_msg();
            assert_eq!(
                parse_msg(),
                dhcp_protocol::Message {
                    op: dhcp_protocol::OpCode::BOOTREQUEST,
                    xid: msg.xid,
                    secs: 0,
                    bdcast_flag: false,
                    ciaddr: Ipv4Addr::UNSPECIFIED,
                    yiaddr: Ipv4Addr::UNSPECIFIED,
                    siaddr: Ipv4Addr::UNSPECIFIED,
                    giaddr: Ipv4Addr::UNSPECIFIED,
                    chaddr: TEST_MAC_ADDRESS,
                    sname: String::new(),
                    file: String::new(),
                    options: vec![
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            TEST_PARAMETER_REQUEST_LIST.into()
                        ),
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPDISCOVER
                        ),
                    ],
                }
            );

            let build_reply = || {
                dhcpv4::server::build_offer(
                    parse_msg(),
                    dhcpv4::server::OfferOptions {
                        offered_ip: YIADDR,
                        server_ip: SERVER_IP,
                        lease_length_config: dhcpv4::configuration::LeaseLength {
                            default_seconds: DEFAULT_LEASE_LENGTH_SECONDS,
                            max_seconds: MAX_LEASE_LENGTH_SECONDS,
                        },
                        // The following fields don't matter for this test, as the
                        // client will read them from the DHCPACK rather than
                        // remembering them from the DHCPOFFER.
                        renewal_time_value: Some(20),
                        rebinding_time_value: Some(30),
                        subnet_mask: std_ip_v4!("255.255.255.0"),
                    },
                    &dhcpv4::server::options_repo([]),
                )
                .expect("dhcp server crate error building offer")
            };

            let bad_reply = dhcp_protocol::Message {
                xid: (u32::from(xid).wrapping_add(1)),
                // Provide a different yiaddr in order to distinguish whether
                // the client correctly discarded this one, since we check which
                // `yiaddr` the client uses as its requested IP address later.
                yiaddr: OTHER_ADDR,
                ..build_reply()
            };

            let good_reply = build_reply();

            let send_reply = |reply: dhcp_protocol::Message| async {
                let dst_ip = reply.yiaddr;
                server_end
                    .send_to(
                        crate::parse::serialize_dhcp_message_to_ip_packet(
                            reply,
                            SERVER_IP,
                            SERVER_PORT,
                            dst_ip,
                            CLIENT_PORT,
                        )
                        .as_ref(),
                        // Note that this is the address the client under test
                        // observes in `recv_from`.
                        TEST_SERVER_MAC_ADDRESS,
                    )
                    .await
                    .expect("send_to on test socket should succeed");
            };

            // The DHCP client should ignore the reply with an incorrect xid.
            send_reply(bad_reply).await;
            send_reply(good_reply).await;
        }
        .fuse();

        pin_mut!(selecting_fut, server_fut);

        let main_future = async move {
            let (selecting_result, ()) = join!(selecting_fut, server_fut);
            selecting_result
        }
        .fuse();
        pin_mut!(main_future);
        let mut executor = fasync::TestExecutor::new();
        let selecting_result = loop {
            match run_until_next_timers_fire(&mut executor, &time, &mut main_future) {
                std::task::Poll::Ready(result) => break result,
                std::task::Poll::Pending => (),
            }
        };

        let requesting = assert_matches!(
            selecting_result,
            Ok(SelectingOutcome::Requesting(requesting)) => requesting,
            "should have successfully transitioned to Requesting"
        );

        assert_eq!(
            requesting,
            Requesting {
                discover_options: DiscoverOptions { xid: requesting.discover_options.xid },
                fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest {
                    server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                        .try_into()
                        .expect("should be specified"),
                    ip_address_lease_time_secs: Some(nonzero_ext::nonzero!(
                        DEFAULT_LEASE_LENGTH_SECONDS
                    )),
                    ip_address_to_request: net_types::ip::Ipv4Addr::from(YIADDR)
                        .try_into()
                        .expect("should be specified"),
                },
                start_time: arbitrary_start_time,
            }
        );
    }
}
