// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the DHCP client state machine.

use crate::deps::{self, DatagramInfo, Instant as _};
use crate::parse::{OptionCodeMap, OptionRequested};
use anyhow::Context as _;
use dhcp_protocol::{AtLeast, AtMostBytes, CLIENT_PORT, SERVER_PORT};
use futures::{
    channel::mpsc, pin_mut, select, FutureExt as _, Stream, StreamExt as _, TryStreamExt as _,
};
use net_types::{ethernet::Mac, SpecifiedAddr, Witness as _};
use rand::Rng as _;
use std::{net::Ipv4Addr, num::NonZeroU32, time::Duration};

/// Unexpected, non-recoverable errors encountered by the DHCP client.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Error encountered while performing a socket operation.
    #[error("error while using socket: {0:?}")]
    Socket(deps::SocketError),
}

/// The reason the DHCP client exited.
pub enum ExitReason {
    /// Executed due to a request for graceful shutdown.
    GracefulShutdown,
}

/// All possible core state machine states from the state-transition diagram in
/// [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
pub enum State<I> {
    /// The default initial state of the state machine (no known
    /// currently-assigned IP address or DHCP server).
    Init(Init),
    /// The Selecting state (broadcasting DHCPDISCOVERs and receiving
    /// DHCPOFFERs).
    Selecting(Selecting<I>),
    /// The Requesting state (broadcasting DHCPREQUESTs and receiving DHCPACKs
    /// and DHCPNAKs).
    Requesting(Requesting<I>),
    /// The Bound state (we actively have a lease and are waiting to transition
    /// to Renewing).
    Bound(Bound<I>),
}

/// The next step to take after running the core state machine for one step.
pub enum Step<I> {
    /// Transition to another state.
    NextState(Transition<I>),
    /// Exit the client.
    Exit(ExitReason),
}

/// A state-transition to execute.
pub struct Transition<I> {
    next_state: State<I>,
    new_lease: Option<NewlyAcquiredLease<I>>,
}

/// A side-effect of a state transition.
#[must_use]
#[derive(Debug)]
pub enum TransitionEffect<I> {
    /// Drop the existing lease.
    DropLease,
    /// Handle a newly-acquired lease.
    HandleNewLease(NewlyAcquiredLease<I>),
    /// No effects need to be performed.
    None,
}

impl<I: deps::Instant> State<I> {
    /// Run the client state machine for one "step".
    pub async fn run<C: deps::Clock<Instant = I>>(
        &self,
        config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        clock: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<Step<I>, Error> {
        match self {
            State::Init(init) => Ok(Step::NextState(Transition {
                next_state: State::Selecting(init.do_init(rng, clock)),
                new_lease: None,
            })),
            State::Selecting(selecting) => match selecting
                .do_selecting(config, packet_socket_provider, rng, clock, stop_receiver)
                .await?
            {
                SelectingOutcome::GracefulShutdown => Ok(Step::Exit(ExitReason::GracefulShutdown)),
                SelectingOutcome::Requesting(requesting) => Ok(Step::NextState(Transition {
                    next_state: State::Requesting(requesting),
                    new_lease: None,
                })),
            },
            State::Requesting(requesting) => {
                match requesting
                    .do_requesting(config, packet_socket_provider, rng, clock, stop_receiver)
                    .await?
                {
                    RequestingOutcome::RanOutOfRetransmits => {
                        tracing::info!(
                            "Returning to Init due to running out of DHCPREQUEST retransmits"
                        );
                        Ok(Step::NextState(Transition {
                            next_state: State::Init(Init::default()),
                            new_lease: None,
                        }))
                    }
                    RequestingOutcome::GracefulShutdown => {
                        Ok(Step::Exit(ExitReason::GracefulShutdown))
                    }
                    RequestingOutcome::Bound(bound, parameters) => {
                        let Bound {
                            yiaddr,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        Ok(Step::NextState(Transition {
                            new_lease: Some(NewlyAcquiredLease {
                                ip_address: *yiaddr,
                                start_time: *start_time,
                                lease_time: *ip_address_lease_time,
                                parameters,
                            }),
                            next_state: State::Bound(bound),
                        }))
                    }
                    RequestingOutcome::Nak(nak) => {
                        // Per RFC 2131 section 3.1: "If the client receives a
                        // DHCPNAK message, the client restarts the
                        // configuration process."
                        tracing::warn!("Returning to Init due to DHCPNAK: {:?}", nak);
                        Ok(Step::NextState(Transition {
                            next_state: State::Init(Init::default()),
                            new_lease: None,
                        }))
                    }
                }
            }
            State::Bound(bound) => {
                let _: &Bound<_> = bound;

                // TODO(https://fxbug.dev/125443): Implement Bound state.
                // In order to unblock testing, rather than panicking here with
                // a todo!, we instead just block until a shutdown request comes
                // in.
                stop_receiver.select_next_some().await;
                Ok(Step::Exit(ExitReason::GracefulShutdown))
            }
        }
    }

    fn has_lease(&self) -> bool {
        match self {
            State::Init(_) => false,
            State::Selecting(_) => false,
            State::Requesting(_) => false,
            State::Bound(_) => true,
        }
    }

    /// Applies a state-transition to `self`, returning the next state and
    /// effects that need to be performed by bindings as a result of the transition.
    pub fn apply(
        &self,
        Transition { next_state, new_lease }: Transition<I>,
    ) -> (State<I>, TransitionEffect<I>) {
        let effect = match new_lease {
            Some(new_lease) => TransitionEffect::HandleNewLease(new_lease),
            None => match (self.has_lease(), next_state.has_lease()) {
                (true, false) => TransitionEffect::DropLease,
                (false, true) => {
                    unreachable!("should already have decided on TransitionEffect::HandleNewLease")
                }
                (false, false) | (true, true) => TransitionEffect::None,
            },
        };
        (next_state, effect)
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
    pub client_hardware_address: Mac,
    /// If set, a unique-on-the-local-network string to be used to identify this
    /// device while negotiating with DHCP servers.
    pub client_identifier:
        Option<AtLeast<2, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<u8>>>>,
    /// Parameters to request from DHCP servers.
    pub requested_parameters: OptionCodeMap<OptionRequested>,
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
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.3.1
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransactionId(
    // While the DHCP RFC does not require that the XID be nonzero, it's helpful
    // to maintain that it is nonzero in order to make it clear that it is set
    // while debugging.
    NonZeroU32,
);

/// The initial state as depicted in the state-transition diagram in [RFC 2131].
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Default)]
pub struct Init {}

impl Init {
    /// Generates a random transaction ID, records the starting time, and
    /// transitions to Selecting.
    fn do_init<C: deps::Clock>(
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
    retransmit_schedule: impl Iterator<Item = Duration>,
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
) -> impl Iterator<Item = Duration> + '_ {
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
            Duration::from_millis(millis)
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

struct OutgoingOptions {
    offered_ip_address_lease_time_secs: Option<NonZeroU32>,
    offered_ip_address: Option<Ipv4Addr>,
    server_identifier: Option<Ipv4Addr>,
    message_type: dhcp_protocol::MessageType,
}

// Populates fields that don't differ between outgoing client messages in the
// same DHCP transaction while the client is not assigned an address.
fn build_outgoing_message_while_not_assigned_address(
    ClientConfig {
        client_hardware_address,
        client_identifier,
        requested_parameters,
        preferred_lease_time_secs,
        requested_ip_address,
    }: &ClientConfig,
    DiscoverOptions { xid: TransactionId(xid) }: &DiscoverOptions,
    OutgoingOptions {
        offered_ip_address_lease_time_secs,
        offered_ip_address,
        server_identifier,
        message_type,
    }: OutgoingOptions,
) -> dhcp_protocol::Message {
    use dhcp_protocol::DhcpOption;
    dhcp_protocol::Message {
        // The following fields are set according to
        // https://www.rfc-editor.org/rfc/rfc2131#section-4.4.1.
        op: dhcp_protocol::OpCode::BOOTREQUEST,
        xid: xid.get(),
        // Must be 0, or the number of seconds since the DHCP process started.
        // Since it has to be the same in DHCPDISCOVER and DHCPREQUEST, it's
        // easiest to have it be 0.
        // TODO(https://fxbug.dev/125232): consider setting this to something
        // else to help DHCP servers prioritize our requests when they're close
        // to expiring.
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
            offered_ip_address_lease_time_secs
                .or(*preferred_lease_time_secs)
                .map(|n| DhcpOption::IpAddressLeaseTime(n.get())),
            offered_ip_address.or(*requested_ip_address).map(DhcpOption::RequestedIpAddress),
            // TODO(https://fxbug.dev/122602): Avoid cloning
            {
                match AtLeast::try_from(requested_parameters.iter_keys().collect::<Vec<_>>()) {
                    Ok(parameters) => Some(DhcpOption::ParameterRequestList(parameters)),
                    Err((
                        dhcp_protocol::SizeConstrainedError::SizeConstraintViolated,
                        parameters,
                    )) => {
                        // This can only have happened because parameters is empty.
                        assert_eq!(parameters, Vec::new());
                        // Thus, we must omit the ParameterRequestList option.
                        None
                    }
                }
            },
            client_identifier.clone().map(DhcpOption::ClientIdentifier),
            server_identifier.map(DhcpOption::ServerIdentifier),
        ]
        .into_iter()
        .flatten()
        .chain([DhcpOption::DhcpMessageType(message_type)])
        .collect(),
    }
}

fn build_discover(
    client_config: &ClientConfig,
    discover_options: &DiscoverOptions,
) -> dhcp_protocol::Message {
    build_outgoing_message_while_not_assigned_address(
        client_config,
        discover_options,
        OutgoingOptions {
            offered_ip_address_lease_time_secs: None,
            offered_ip_address: None,
            server_identifier: None,
            message_type: dhcp_protocol::MessageType::DHCPDISCOVER,
        },
    )
}

#[derive(Debug)]
pub(crate) enum SelectingOutcome<I> {
    GracefulShutdown,
    Requesting(Requesting<I>),
}

/// The Selecting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
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
    async fn do_selecting<C: deps::Clock<Instant = I>>(
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

        let ClientConfig {
            client_hardware_address: _,
            client_identifier: _,
            requested_parameters,
            preferred_lease_time_secs: _,
            requested_ip_address: _,
        } = client_config;

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
            /* dest= */ Mac::BROADCAST,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];
        let offer_fields_stream = recv_stream(&socket, &mut recv_buf, |packet, src_addr| {
            // We don't care about the src addr of incoming offers, because we
            // identify DHCP servers via the Server Identifier option.
            let _: Mac = src_addr;
            let message = crate::parse::parse_dhcp_message_from_ip_packet(packet, CLIENT_PORT)
                .context("error while parsing DHCP message from IP packet")?;
            validate_message(discover_options, client_config, &message)
                .context("invalid DHCP message")?;
            crate::parse::fields_to_retain_from_selecting(requested_parameters, message)
                .context("error while retrieving fields to use in DHCPREQUEST from DHCP message")
        })
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(fields) => Some(fields),
                Err(error) => {
                    tracing::warn!("discarding incoming packet: {:?}", error);
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
    WrongChaddr { expected: Mac, actual: Mac },
}

fn validate_message(
    DiscoverOptions { xid: TransactionId(my_xid) }: &DiscoverOptions,
    ClientConfig {
        client_hardware_address: my_chaddr,
        client_identifier: _,
        requested_parameters: _,
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
pub(crate) enum RequestingOutcome<I> {
    RanOutOfRetransmits,
    GracefulShutdown,
    Bound(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    Nak(crate::parse::FieldsToRetainFromNak),
}

/// The Requesting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug, PartialEq)]
pub struct Requesting<I> {
    discover_options: DiscoverOptions,
    fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest,
    start_time: I,
}

// Per RFC 2131, section 3.1: "a client retransmitting as described in section
// 4.1 might retransmit the DHCPREQUEST message four times, for a total delay of
// 60 seconds, before restarting the initialization procedure".
const NUM_REQUEST_RETRANSMITS: usize = 4;

impl<I: deps::Instant> Requesting<I> {
    /// Executes the Requesting state.
    ///
    /// Transmits (and retransmits, if necessary) DHCPREQUEST messages, and
    /// receives DHCPACK and DHCPNAK messages, on a packet socket. Upon
    /// receiving a DHCPACK, transitions to Bound.
    async fn do_requesting<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<RequestingOutcome<I>, Error> {
        let socket = packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;
        let Requesting { discover_options, fields_from_offer_to_use_in_request, start_time } = self;
        let message =
            build_request(client_config, discover_options, fields_from_offer_to_use_in_request);

        let message = crate::parse::serialize_dhcp_message_to_ip_packet(
            message,
            Ipv4Addr::UNSPECIFIED, // src_ip
            CLIENT_PORT,
            Ipv4Addr::BROADCAST, // dst_ip
            SERVER_PORT,
        );

        let send_fut = send_with_retransmits(
            time,
            default_retransmit_schedule(rng.get_rng()).take(NUM_REQUEST_RETRANSMITS),
            message.as_ref(),
            &socket,
            Mac::BROADCAST,
        )
        .fuse();

        let ClientConfig {
            client_hardware_address: _,
            client_identifier: _,
            requested_parameters,
            preferred_lease_time_secs: _,
            requested_ip_address: _,
        } = client_config;

        let mut recv_buf = [0u8; BUFFER_SIZE];

        let ack_or_nak_stream = recv_stream(&socket, &mut recv_buf, |packet, src_addr| {
            // We don't care about the src addr of incoming messages, because we
            // identify DHCP servers via the Server Identifier option.
            let _: Mac = src_addr;
            let message = crate::parse::parse_dhcp_message_from_ip_packet(packet, CLIENT_PORT)
                .context("error while parsing DHCP message from IP packet")?;
            validate_message(discover_options, client_config, &message)
                .context("invalid DHCP message")?;

            crate::parse::fields_to_retain_from_requesting(requested_parameters, message)
                .context("error extracting needed fields from DHCP message during Requesting")
        })
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(msg) => Some(msg),
                Err(error) => {
                    tracing::warn!("discarding incoming packet: {:?}", error);
                    None
                }
            })
        })
        .fuse();

        pin_mut!(send_fut, ack_or_nak_stream);
        let fields_to_retain = select! {
            send_requests_result = send_fut => {
                send_requests_result?;
                return Ok(RequestingOutcome::RanOutOfRetransmits)
            },
            () = stop_receiver.select_next_some() => {
                return Ok(RequestingOutcome::GracefulShutdown)
            },
            fields_to_retain_result = ack_or_nak_stream.select_next_some() => {
                fields_to_retain_result?
            }
        };

        match fields_to_retain {
            crate::parse::IncomingMessageDuringRequesting::Ack(ack) => {
                let crate::parse::FieldsToRetainFromAck {
                    yiaddr,
                    server_identifier,
                    ip_address_lease_time_secs,
                    renewal_time_value_secs,
                    rebinding_time_value_secs,
                    parameters,
                } = ack;
                Ok(RequestingOutcome::Bound(
                    Bound {
                        yiaddr,
                        server_identifier: server_identifier.unwrap_or({
                            let crate::parse::FieldsFromOfferToUseInRequest {
                                server_identifier,
                                ip_address_lease_time_secs: _,
                                ip_address_to_request: _,
                            } = fields_from_offer_to_use_in_request;
                            *server_identifier
                        }),
                        ip_address_lease_time: Duration::from_secs(
                            ip_address_lease_time_secs.get().into(),
                        ),
                        renewal_time: renewal_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        rebinding_time: rebinding_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        start_time: *start_time,
                    },
                    parameters,
                ))
            }
            crate::parse::IncomingMessageDuringRequesting::Nak(nak) => {
                Ok(RequestingOutcome::Nak(nak))
            }
        }
    }
}

fn build_request(
    client_config: &ClientConfig,
    discover_options: &DiscoverOptions,
    crate::parse::FieldsFromOfferToUseInRequest {
        server_identifier,
        ip_address_lease_time_secs,
        ip_address_to_request,
    }: &crate::parse::FieldsFromOfferToUseInRequest,
) -> dhcp_protocol::Message {
    build_outgoing_message_while_not_assigned_address(
        client_config,
        discover_options,
        OutgoingOptions {
            offered_ip_address_lease_time_secs: *ip_address_lease_time_secs,
            offered_ip_address: Some(ip_address_to_request.get().into()),
            server_identifier: Some(server_identifier.get().into()),
            message_type: dhcp_protocol::MessageType::DHCPREQUEST,
        },
    )
}

/// The Bound state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4.1
#[derive(Debug, PartialEq)]
pub struct Bound<I> {
    yiaddr: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    server_identifier: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    ip_address_lease_time: Duration,
    start_time: I,
    renewal_time: Option<Duration>,
    rebinding_time: Option<Duration>,
}

/// A newly-acquired DHCP lease.
#[derive(Debug)]
pub struct NewlyAcquiredLease<I> {
    /// The IP address acquired.
    pub ip_address: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    /// The start time of the lease.
    pub start_time: I,
    /// The length of the lease.
    pub lease_time: Duration,
    /// Configuration parameters acquired from the server. Guaranteed to be a
    /// subset of the parameters requested in the `parameter_request_list` in
    /// `ClientConfig`.
    pub parameters: Vec<dhcp_protocol::DhcpOption>,
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
    use futures::{join, Future};
    use itertools::Itertools as _;
    use net_declare::{net::prefix_length_v4, net_mac, std_ip_v4};
    use net_types::ethernet::Mac;
    use net_types::ip::{Ipv4, PrefixLength};
    use std::cell::RefCell;
    use std::rc::Rc;
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

    const TEST_MAC_ADDRESS: Mac = net_mac!("01:01:01:01:01:01");
    const TEST_SERVER_MAC_ADDRESS: Mac = net_mac!("02:02:02:02:02:02");
    const OTHER_MAC_ADDRESS: Mac = net_mac!("03:03:03:03:03:03");

    const SERVER_IP: Ipv4Addr = std_ip_v4!("192.168.1.1");
    const YIADDR: Ipv4Addr = std_ip_v4!("198.168.1.5");
    const OTHER_ADDR: Ipv4Addr = std_ip_v4!("198.168.1.6");
    const DEFAULT_LEASE_LENGTH_SECONDS: u32 = 100;
    const MAX_LEASE_LENGTH_SECONDS: u32 = 200;
    const TEST_PREFIX_LENGTH: PrefixLength<Ipv4> = prefix_length_v4!(24);

    fn test_requested_parameters() -> OptionCodeMap<OptionRequested> {
        use dhcp_protocol::OptionCode;
        [
            (OptionCode::SubnetMask, OptionRequested::Required),
            (OptionCode::Router, OptionRequested::Optional),
            (OptionCode::DomainNameServer, OptionRequested::Optional),
        ]
        .into_iter()
        .collect::<OptionCodeMap<_>>()
    }

    fn test_parameter_values_excluding_subnet_mask() -> [dhcp_protocol::DhcpOption; 2] {
        [
            dhcp_protocol::DhcpOption::Router([SERVER_IP].into()),
            dhcp_protocol::DhcpOption::DomainNameServer([SERVER_IP, std_ip_v4!("8.8.8.8")].into()),
        ]
    }

    fn test_parameter_values() -> impl IntoIterator<Item = dhcp_protocol::DhcpOption> {
        std::iter::once(dhcp_protocol::DhcpOption::SubnetMask(TEST_PREFIX_LENGTH))
            .chain(test_parameter_values_excluding_subnet_mask())
    }

    fn test_client_config() -> ClientConfig {
        ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            requested_parameters: test_requested_parameters(),
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

    fn run_with_accelerated_time<F>(
        executor: &mut fasync::TestExecutor,
        time: &Rc<RefCell<FakeTimeController>>,
        main_future: &mut F,
    ) -> F::Output
    where
        F: Future + Unpin,
    {
        loop {
            match run_until_next_timers_fire(executor, time, main_future) {
                std::task::Poll::Ready(result) => break result,
                std::task::Poll::Pending => (),
            }
        }
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

        run_with_accelerated_time(&mut executor, time, &mut main_future);

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let selecting_result = selecting_fut.now_or_never().expect(
            "selecting_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(selecting_result, Ok(SelectingOutcome::GracefulShutdown));
    }

    struct VaryingOutgoingMessageFields {
        xid: u32,
        options: Vec<dhcp_protocol::DhcpOption>,
    }

    #[track_caller]
    fn assert_outgoing_message(
        got_message: &dhcp_protocol::Message,
        fields: VaryingOutgoingMessageFields,
    ) {
        let VaryingOutgoingMessageFields { xid, options } = fields;
        let want_message = dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREQUEST,
            xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: TEST_MAC_ADDRESS,
            sname: String::new(),
            file: String::new(),
            options,
        };
        assert_eq!(got_message, &want_message);
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

        // These are the time ranges in which we expect to see messages from the
        // DHCP client. They are ranges in order to account for randomized
        // delays.
        const EXPECTED_RANGES: [(u64, u64); 7] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33), (63, 65), (63, 65)];

        let receive_fut = async {
            let mut previous_time = std::time::Duration::from_secs(0);

            for (start, end) in EXPECTED_RANGES {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(address, Mac::BROADCAST);

                let msg = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet should parse as DHCP message");

                assert_outgoing_message(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPDISCOVER,
                            ),
                        ],
                    },
                );

                let received_time = time.now();

                let duration_range =
                    std::time::Duration::from_secs(start)..=std::time::Duration::from_secs(end);
                assert!(duration_range.contains(&(received_time - previous_time)));

                previous_time = received_time;
            }
        }
        .fuse();

        pin_mut!(selecting_fut, receive_fut);

        let main_future = async {
            select! {
                _ = selecting_fut => unreachable!("should keep retransmitting DHCPDISCOVER forever"),
                () = receive_fut => (),
            }
        };
        pin_mut!(main_future);

        run_with_accelerated_time(&mut executor, time, &mut main_future);
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
        message_chaddr: Mac,
    ) -> Result<(), ValidateMessageError> {
        let discover_options = DiscoverOptions { xid: TransactionId(XID) };
        let client_config = ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            requested_parameters: test_requested_parameters(),
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

        let (server_end, client_end) = FakeSocket::<Mac>::new_pair();
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
                assert_eq!(dst_addr, Mac::BROADCAST);

                server_end
                    .send_to(b"hello", OTHER_MAC_ADDRESS)
                    .await
                    .expect("send_to with garbage data should succeed");
            }

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, Mac::BROADCAST);

            // `dhcp_protocol::Message` intentionally doesn't implement `Clone`,
            // so we re-parse instead for testing purposes.
            let parse_msg = || {
                crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet on test socket should parse as DHCP message")
            };

            let msg = parse_msg();
            assert_outgoing_message(
                &parse_msg(),
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPDISCOVER,
                        ),
                    ],
                },
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
                        subnet_mask: TEST_PREFIX_LENGTH,
                    },
                    &dhcpv4::server::options_repo(test_parameter_values()),
                )
                .expect("dhcp server crate error building offer")
            };

            let reply_with_wrong_xid = dhcp_protocol::Message {
                xid: (u32::from(xid).wrapping_add(1)),
                // Provide a different yiaddr in order to distinguish whether
                // the client correctly discarded this one, since we check which
                // `yiaddr` the client uses as its requested IP address later.
                yiaddr: OTHER_ADDR,
                ..build_reply()
            };

            let reply_without_subnet_mask = {
                let mut reply = build_reply();
                let options = std::mem::take(&mut reply.options);
                let (subnet_masks, other_options): (Vec<_>, Vec<_>) =
                    options.into_iter().partition_map(|option| match option {
                        dhcp_protocol::DhcpOption::SubnetMask(_) => itertools::Either::Left(option),
                        _ => itertools::Either::Right(option),
                    });
                assert_matches!(
                    &subnet_masks[..],
                    &[dhcp_protocol::DhcpOption::SubnetMask(TEST_PREFIX_LENGTH)]
                );
                reply.options = other_options;

                // Provide a different yiaddr in order to distinguish whether
                // the client correctly discarded this one, since we check which
                // `yiaddr` the client uses as its requested IP address later.
                reply.yiaddr = OTHER_ADDR;
                reply
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
            send_reply(reply_with_wrong_xid).await;

            // The DHCP client should ignore the reply without a subnet mask.
            send_reply(reply_without_subnet_mask).await;

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
        let selecting_result = run_with_accelerated_time(&mut executor, &time, &mut main_future);

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

    fn build_test_requesting_state() -> Requesting<std::time::Duration> {
        Requesting {
            discover_options: DiscoverOptions { xid: TransactionId(nonzero_ext::nonzero!(1u32)) },
            start_time: std::time::Duration::from_secs(0),
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
        }
    }

    #[test]
    fn do_requesting_obeys_graceful_shutdown() {
        initialize_logging();

        let time = FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (_server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();
        pin_mut!(requesting_fut);

        let mut executor = fasync::TestExecutor::new();
        assert_matches!(executor.run_until_stalled(&mut requesting_fut), std::task::Poll::Pending);

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let requesting_result = requesting_fut.now_or_never().expect(
            "requesting_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(requesting_result, Ok(RequestingOutcome::GracefulShutdown));
    }

    #[test]
    fn do_requesting_sends_requests() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        // These are the time ranges in which we expect to see messages from the
        // DHCP client. They are ranges in order to account for randomized
        // delays.
        const EXPECTED_RANGES: [(u64, u64); NUM_REQUEST_RETRANSMITS + 1] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33)];

        let receive_fut = async {
            let mut previous_time = std::time::Duration::from_secs(0);

            for (start, end) in EXPECTED_RANGES {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(address, Mac::BROADCAST);

                let msg = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet should parse as DHCP message");

                assert_outgoing_message(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                                DEFAULT_LEASE_LENGTH_SECONDS,
                            ),
                            dhcp_protocol::DhcpOption::RequestedIpAddress(YIADDR),
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPREQUEST,
                            ),
                        ],
                    },
                );

                let received_time = time.now();

                let duration_range =
                    std::time::Duration::from_secs(start)..=std::time::Duration::from_secs(end);
                assert!(duration_range.contains(&(received_time - previous_time)));

                previous_time = received_time;
            }
        }
        .fuse();

        pin_mut!(requesting_fut, receive_fut);

        let main_future = async { join!(requesting_fut, receive_fut) };
        pin_mut!(main_future);

        let (requesting_result, ()) =
            run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(RequestingOutcome::RanOutOfRetransmits));
    }

    struct VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr,
        options: Vec<dhcp_protocol::DhcpOption>,
    }

    fn build_incoming_message(
        xid: u32,
        fields: VaryingIncomingMessageFields,
    ) -> dhcp_protocol::Message {
        let VaryingIncomingMessageFields { yiaddr, options } = fields;

        dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREPLY,
            xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: TEST_MAC_ADDRESS,
            sname: String::new(),
            file: String::new(),
            options,
        }
    }

    const NAK_MESSAGE: &str = "something went wrong";

    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RequestingOutcome::Bound(Bound {
        yiaddr: net_types::ip::Ipv4Addr::from(YIADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect()) ; "transitions to Bound after receiving DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values_excluding_subnet_mask())
        .collect(),
    } => RequestingOutcome::RanOutOfRetransmits ; "ignores replies lacking required option SubnetMask")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr::UNSPECIFIED,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPNAK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::Message(NAK_MESSAGE.to_owned()),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RequestingOutcome::Nak(crate::parse::FieldsToRetainFromNak {
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        message: Some(NAK_MESSAGE.to_owned()),
        client_identifier: None,
    }) ; "transitions to Init after receiving DHCPNAK")]
    fn do_requesting_transitions_on_reply(
        incoming_message: VaryingIncomingMessageFields,
    ) -> RequestingOutcome<std::time::Duration> {
        initialize_logging();

        let time = &FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                time,
                &mut stop_receiver,
            )
            .fuse();

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, Mac::BROADCAST);

            // `dhcp_protocol::Message` intentionally doesn't implement `Clone`,
            // so we re-parse instead for testing purposes.
            let parse_msg = || {
                crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet on test socket should parse as DHCP message")
            };

            let msg = parse_msg();

            assert_outgoing_message(
                &parse_msg(),
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::IpAddressLeaseTime(DEFAULT_LEASE_LENGTH_SECONDS),
                        dhcp_protocol::DhcpOption::RequestedIpAddress(YIADDR),
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                        dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPREQUEST,
                        ),
                    ],
                },
            );

            let reply = build_incoming_message(msg.xid, incoming_message);

            server_end
                .send_to(
                    crate::parse::serialize_dhcp_message_to_ip_packet(
                        reply,
                        SERVER_IP,
                        SERVER_PORT,
                        YIADDR,
                        CLIENT_PORT,
                    )
                    .as_ref(),
                    // Note that this is the address the client under test
                    // observes in `recv_from`.
                    TEST_SERVER_MAC_ADDRESS,
                )
                .await
                .expect("send_to on test socket should succeed");
        }
        .fuse();

        pin_mut!(requesting_fut, server_fut);

        let main_future = async move {
            let (requesting_result, ()) = join!(requesting_fut, server_fut);
            requesting_result
        }
        .fuse();

        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let requesting_result = run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(outcome) => outcome)
    }

    fn build_test_bound_state() -> Bound<std::time::Duration> {
        Bound {
            yiaddr: net_types::ip::Ipv4Addr::from(YIADDR).try_into().expect("should be specified"),
            server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                .try_into()
                .expect("should be specified"),
            ip_address_lease_time: Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            start_time: std::time::Duration::from_secs(0),
            renewal_time: None,
            rebinding_time: None,
        }
    }

    fn build_test_newly_acquired_lease() -> NewlyAcquiredLease<Duration> {
        NewlyAcquiredLease {
            ip_address: net_types::ip::Ipv4Addr::from(YIADDR)
                .try_into()
                .expect("should be specified"),
            start_time: std::time::Duration::from_secs(0),
            lease_time: Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            parameters: Vec::new(),
        }
    }

    #[test_case(
        (
            State::Init(Init::default()),
            Transition {
                next_state: State::Bound(build_test_bound_state()),
                new_lease: Some(build_test_newly_acquired_lease())
            }
        ) => matches TransitionEffect::HandleNewLease(_);
        "yields newly-acquired lease effect"
    )]
    #[test_case(
        (
            State::Bound(build_test_bound_state()),
            Transition {
                next_state: State::Init(Init::default()),
                new_lease: None,
            }
        ) => matches TransitionEffect::DropLease;
        "recognizes loss of lease"
    )]
    fn apply_transition(
        (state, transition): (State<Duration>, Transition<Duration>),
    ) -> TransitionEffect<Duration> {
        let (_next_state, effect) = state.apply(transition);
        effect
    }
}
