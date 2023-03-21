// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the DHCP client state machine.

use todo_unused::todo_unused;

#[todo_unused("https://fxbug.dev/81593")]
use crate::deps::{self, Instant as _};
#[todo_unused("https://fxbug.dev/81593")]
use dhcp_protocol::{AtLeast, AtMostBytes};
#[todo_unused("https://fxbug.dev/81593")]
use futures::{pin_mut, select, FutureExt as _};
#[todo_unused("https://fxbug.dev/81593")]
use rand::Rng as _;
#[todo_unused("https://fxbug.dev/81593")]
use std::{net::Ipv4Addr, num::NonZeroU32};

#[todo_unused("https://fxbug.dev/81593")]
const SERVER_PORT: std::num::NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT);

#[todo_unused("https://fxbug.dev/81593")]
const CLIENT_PORT: std::num::NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::CLIENT_PORT);

/// Unexpected, non-recoverable errors encountered by the DHCP client.
#[todo_unused("https://fxbug.dev/81593")]
#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("error while using socket: {0:?}")]
    Socket(deps::SocketError),
}

/// Configuration for the DHCP client to be used while negotiating with DHCP
/// servers.
#[todo_unused("https://fxbug.dev/81593")]
#[derive(Clone)]
struct ClientConfig {
    /// The hardware address of the interface on which the DHCP client is run.
    client_hardware_address: net_types::ethernet::Mac,
    /// If set, a unique-on-the-local-network string to be used to identify this
    /// device while negotiating with DHCP servers.
    client_identifier: Option<AtLeast<2, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<u8>>>>,
    /// A list of parameters to request from DHCP servers.
    parameter_request_list: Option<
        AtLeast<1, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<dhcp_protocol::OptionCode>>>,
    >,
    /// If set, the preferred IP address lease time in seconds.
    preferred_lease_time_secs: Option<NonZeroU32>,
    /// If set, the IP address to request from DHCP servers.
    requested_ip_address: Option<Ipv4Addr>,
}

#[todo_unused("https://fxbug.dev/81593")]
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
#[todo_unused("https://fxbug.dev/81593")]
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransactionId(
    // While the DHCP RFC does not require that the XID be nonzero, it's helpful
    // to maintain that it is nonzero in order to make it clear that it is set
    // while debugging.
    NonZeroU32,
);

/// The initial state as depicted in the state-transition diagram in [RFC 2131].
/// [RFC 2131]: https://www.rfc-editor.org/rfc/inline-errata/rfc2131.html#section-4.4
#[todo_unused("https://fxbug.dev/81593")]
struct Init {}

#[todo_unused("https://fxbug.dev/81593")]
impl Init {
    /// Generates a random transaction ID, and transitions to Selecting.
    fn do_init(&self, rng: &mut impl deps::RngProvider) -> Selecting {
        let discover_options = DiscoverOptions {
            xid: TransactionId(NonZeroU32::new(rng.get_rng().gen_range(1..=u32::MAX)).unwrap()),
        };
        Selecting { discover_options }
    }
}

#[todo_unused("https://fxbug.dev/81593")]
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
        socket.send_to(message, dest.clone()).await.map_err(Error::Socket)?;
    }
    Ok(())
}

#[todo_unused("https://fxbug.dev/81593")]
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

#[todo_unused("https://fxbug.dev/81593")]
// This is assumed to be an appropriate buffer size due to Ethernet's common MTU
// of 1500 bytes.
const BUFFER_SIZE: usize = 1500;

#[todo_unused("https://fxbug.dev/81593")]
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

#[todo_unused("https://fxbug.dev/81593")]
#[derive(Debug)]
enum SelectingOutcome {
    // TODO(https://fxbug.dev/123520): Implement receiving offers.
}

/// The Selecting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://www.rfc-editor.org/rfc/inline-errata/rfc2131.html#section-4.4
#[todo_unused("https://fxbug.dev/81593")]
struct Selecting {
    discover_options: DiscoverOptions,
}

#[todo_unused("https://fxbug.dev/81593")]
impl Selecting {
    /// Executes the Selecting state.
    ///
    /// Transmits (and retransmits, if necessary) DHCPDISCOVER messages, and
    /// receives DHCPOFFER messages, on a packet socket. Tries to select a
    /// DHCPOFFER. If successful, transitions to Requesting.
    async fn do_selecting(
        &self,
        client_config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        time: &impl deps::Clock,
    ) -> Result<SelectingOutcome, Error> {
        let socket = packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;
        let Selecting { discover_options } = self;
        let message = build_discover(client_config, discover_options);

        let message = crate::parse::serialize_dhcp_message_to_ip_packet(
            message,
            Ipv4Addr::UNSPECIFIED, // src_ip
            CLIENT_PORT,
            Ipv4Addr::BROADCAST, // dst_ip
            SERVER_PORT,
        );

        // TODO(https://fxbug.dev/123520): Interrupt this when receiving offers.
        send_with_retransmits(
            time,
            default_retransmit_schedule(rng.get_rng()),
            message.as_ref(),
            &socket,
            /* dest= */ net_types::ethernet::Mac::BROADCAST,
        )
        .await?;

        unreachable!("should never stop retransmitting DHCPDISCOVER unless we hit an error");
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::deps::testutil::{
        run_until_next_timers_fire, FakeRngProvider, FakeSocket, FakeSocketProvider,
        FakeTimeController,
    };
    use crate::deps::{Clock as _, DatagramInfo, Socket as _};
    use fuchsia_async as fasync;
    use futures::StreamExt as _;

    fn initialize_logging() {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::INFO)
            .finish();

        // Intentionally don't use the result here, since it'll succeed with the
        // first test case that calls it and fail with the others.
        let _: Result<_, _> = tracing::subscriber::set_global_default(subscriber);
    }

    const TEST_MAC_ADDRESS: net_types::ethernet::Mac = net_declare::net_mac!("01:02:03:04:05:06");

    const TEST_PARAMETER_REQUEST_LIST: [dhcp_protocol::OptionCode; 3] = [
        dhcp_protocol::OptionCode::SubnetMask,
        dhcp_protocol::OptionCode::Router,
        dhcp_protocol::OptionCode::DomainNameServer,
    ];

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
        let Selecting { discover_options: DiscoverOptions { xid: xid_a } } =
            Init {}.do_init(&mut rng);
        let Selecting { discover_options: DiscoverOptions { xid: xid_b } } =
            Init {}.do_init(&mut rng);
        assert_ne!(xid_a, xid_b);
    }

    #[test]
    fn do_selecting_sends_discover() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let selecting = Selecting {
            discover_options: DiscoverOptions { xid: TransactionId(nonzero_ext::nonzero!(1u32)) },
        };
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let selecting_fut =
            selecting.do_selecting(&client_config, &test_socket_provider, &mut rng, &time).fuse();

        let time = &time;

        const EXPECTED_RANGES: [(u64, u64); 7] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33), (63, 65), (63, 65)];

        let receive_fut = async_stream::stream! {
            loop {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } =
                    server_end.recv_from(&mut recv_buf).await.unwrap();

                assert_eq!(address, net_types::ethernet::Mac::BROADCAST);

                let msg = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT),
                )
                .unwrap();

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
}
