// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Duplicate Address Detection.

use core::num::{NonZero, NonZeroU16};

use arrayvec::ArrayVec;
use log::debug;
use net_types::ip::{Ipv4, Ipv6, Ipv6Addr};
use net_types::{MulticastAddr, UnicastAddr, Witness as _};
use netstack3_base::{
    AnyDevice, CoreEventContext, CoreTimerContext, DeviceIdContext, EventContext, HandleableTimer,
    IpAddressId as _, IpDeviceAddressIdContext, RngContext, StrongDeviceIdentifier as _,
    TimerBindingsTypes, TimerContext, WeakDeviceIdentifier,
};
use packet_formats::icmp::ndp::options::{NdpNonce, MIN_NONCE_LENGTH};
use packet_formats::icmp::ndp::NeighborSolicitation;
use packet_formats::utils::NonZeroDuration;

use crate::internal::device::nud::DEFAULT_MAX_MULTICAST_SOLICIT;
use crate::internal::device::state::Ipv6DadState;
use crate::internal::device::{IpAddressState, IpDeviceIpExt, WeakIpAddressId};

/// A timer ID for duplicate address detection.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct DadTimerId<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> {
    pub(crate) device_id: D,
    pub(crate) addr: A,
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> DadTimerId<D, A> {
    pub(super) fn device_id(&self) -> &D {
        let Self { device_id, addr: _ } = self;
        device_id
    }

    /// Creates a new [`DadTimerId`]  for `device_id` and `addr`.
    #[cfg(any(test, feature = "testutils"))]
    pub fn new(device_id: D, addr: A) -> Self {
        Self { device_id, addr }
    }
}

/// A reference to the DAD address state.
pub struct DadAddressStateRef<'a, CC, BT: DadBindingsTypes> {
    /// A mutable reference to an address' state.
    ///
    /// `None` if the address is not recognized.
    pub dad_state: &'a mut Ipv6DadState<BT>,
    /// The execution context available with the address's DAD state.
    pub core_ctx: &'a mut CC,
}

/// Holds references to state associated with duplicate address detection.
pub struct DadStateRef<'a, CC, BT: DadBindingsTypes> {
    /// A reference to the DAD address state.
    pub state: DadAddressStateRef<'a, CC, BT>,
    /// The time between DAD message retransmissions.
    pub retrans_timer: &'a NonZeroDuration,
    /// The maximum number of DAD messages to send.
    pub max_dad_transmits: &'a Option<NonZeroU16>,
}

/// The execution context while performing DAD.
pub trait DadAddressContext<BC>: IpDeviceAddressIdContext<Ipv6> {
    /// Calls the function with a mutable reference to the address's assigned
    /// flag.
    fn with_address_assigned<O, F: FnOnce(&mut bool) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        cb: F,
    ) -> O;

    /// Joins the multicast group on the device.
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    );

    /// Leaves the multicast group on the device.
    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    );
}

/// The execution context for DAD.
pub trait DadContext<BC: DadBindingsTypes>:
    IpDeviceAddressIdContext<Ipv6>
    + DeviceIdContext<AnyDevice>
    + CoreTimerContext<DadTimerId<Self::WeakDeviceId, Self::WeakAddressId>, BC>
    + CoreEventContext<DadEvent<Self::DeviceId>>
{
    /// The inner address context.
    type DadAddressCtx<'a>: DadAddressContext<
        BC,
        DeviceId = Self::DeviceId,
        AddressId = Self::AddressId,
    >;

    /// Calls the function with the DAD state associated with the address.
    fn with_dad_state<O, F: FnOnce(DadStateRef<'_, Self::DadAddressCtx<'_>, BC>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        cb: F,
    ) -> O;

    /// Sends an NDP Neighbor Solicitation message for DAD to the local-link.
    ///
    /// The message will be sent with the unspecified (all-zeroes) source
    /// address.
    fn send_dad_packet(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
        nonce: NdpNonce<&[u8]>,
    ) -> Result<(), ()>;
}

// Chosen somewhat arbitrarily. It's unlikely we need to store many
// previously-used nonces given that we'll probably only ever see the most
// recently used nonce looped back at us.
const MAX_DAD_PROBE_NONCES_STORED: usize = 4;

/// A data structure storing a limited number of `NdpNonce`s.
#[derive(Default, Debug)]
pub struct NonceCollection {
    nonces: ArrayVec<[u8; MIN_NONCE_LENGTH], MAX_DAD_PROBE_NONCES_STORED>,
}

impl NonceCollection {
    /// Given an `rng` source, generates a new unique nonce and stores it,
    /// deleting the oldest nonce if there is no space remaining.
    pub fn evicting_create_and_store_nonce(
        &mut self,
        mut rng: impl rand::Rng,
    ) -> [u8; MIN_NONCE_LENGTH] {
        let Self { nonces } = self;
        loop {
            let nonce: [u8; MIN_NONCE_LENGTH] = rng.gen();
            if nonces.iter().any(|stored_nonce| stored_nonce == &nonce) {
                continue;
            }

            if nonces.remaining_capacity() == 0 {
                let _: [u8; MIN_NONCE_LENGTH] = nonces.remove(0);
            }
            nonces.push(nonce.clone());
            break nonce;
        }
    }

    /// Checks if `nonce` is in the collection.
    pub fn contains(&self, nonce: &[u8]) -> bool {
        if nonce.len() != MIN_NONCE_LENGTH {
            return false;
        }

        let Self { nonces } = self;
        nonces.iter().any(|stored_nonce| stored_nonce == &nonce)
    }
}

#[derive(Debug, Eq, Hash, PartialEq)]
/// Events generated by duplicate address detection.
pub enum DadEvent<DeviceId> {
    /// Duplicate address detection completed and the address is assigned.
    AddressAssigned {
        /// Device the address belongs to.
        device: DeviceId,
        /// The address that moved to the assigned state.
        addr: UnicastAddr<Ipv6Addr>,
    },
}

/// The bindings types for DAD.
pub trait DadBindingsTypes: TimerBindingsTypes {}
impl<BT> DadBindingsTypes for BT where BT: TimerBindingsTypes {}

/// The bindings execution context for DAD.
///
/// The type parameter `E` is tied by [`DadContext`] so that [`DadEvent`] can be
/// transformed into an event that is more meaningful to bindings.
pub trait DadBindingsContext<E>:
    DadBindingsTypes + TimerContext + EventContext<E> + RngContext
{
}
impl<E, BC> DadBindingsContext<E> for BC where
    BC: DadBindingsTypes + TimerContext + EventContext<E> + RngContext
{
}

/// The result of looking up an address and nonce from a neighbor solicitation
/// in the DAD state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DadAddressStateLookupResult {
    Uninitialized,
    Tentative { matched_nonce: bool },
    Assigned,
}

/// An implementation for Duplicate Address Detection.
pub trait DadHandler<I: IpDeviceIpExt, BC>:
    DeviceIdContext<AnyDevice> + IpDeviceAddressIdContext<I>
{
    /// The initial address state for newly added addresses.
    // TODO(https://fxbug.dev/42077260): This can probably be removed when we
    // can do DAD over IPv4.
    const INITIAL_ADDRESS_STATE: IpAddressState;

    /// Starts duplicate address detection.
    ///
    /// # Panics
    ///
    /// Panics if tentative state for the address is not found.
    fn start_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    );

    /// Stops duplicate address detection.
    ///
    /// Does nothing if DAD is not being performed on the address.
    fn stop_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    );

    /// Handles an incoming neighbor solicitation that was determined to be sent
    /// as part of a node (possibly ourselves) performing duplicate address
    /// detection.
    ///
    /// If the incoming solicitation is determined to be a looped-back probe
    /// that we ourselves sent, updates DAD state accordingly to send additional
    /// probes.
    fn handle_incoming_dad_neighbor_solicitation(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        nonce: Option<NdpNonce<&'_ [u8]>>,
    ) -> DadAddressStateLookupResult;
}

enum DoDadVariation {
    Start,
    Continue,
}

fn do_duplicate_address_detection<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: &CC::AddressId,
    variation: DoDadVariation,
) {
    let nonce_if_should_send_message = core_ctx.with_dad_state(
        device_id,
        addr,
        |DadStateRef { state, retrans_timer, max_dad_transmits }| {
            let DadAddressStateRef { dad_state, core_ctx } = state;

            match variation {
                DoDadVariation::Start => {
                    // Mark the address as tentative/unassigned before joining
                    // the group so that the address is not used as the source
                    // for any outgoing MLD message.
                    *dad_state = Ipv6DadState::Tentative {
                        dad_transmits_remaining: *max_dad_transmits,
                        timer: CC::new_timer(
                            bindings_ctx,
                            DadTimerId { device_id: device_id.downgrade(), addr: addr.downgrade() },
                        ),
                        nonces: Default::default(),
                        added_extra_transmits_after_detecting_looped_back_ns: false,
                    };
                    core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = false);

                    // As per RFC 4861 section 5.6.2,
                    //
                    //   Before sending a Neighbor Solicitation, an interface MUST join
                    //   the all-nodes multicast address and the solicited-node
                    //   multicast address of the tentative address.
                    //
                    // Note that we join the all-nodes multicast address on interface
                    // enable.
                    core_ctx.join_multicast_group(
                        bindings_ctx,
                        device_id,
                        addr.addr().addr().to_solicited_node_address(),
                    );
                }
                DoDadVariation::Continue => {}
            }

            let (remaining, timer, nonces) = match dad_state {
                Ipv6DadState::Tentative {
                    dad_transmits_remaining,
                    timer,
                    nonces,
                    added_extra_transmits_after_detecting_looped_back_ns: _,
                } => (dad_transmits_remaining, timer, nonces),
                Ipv6DadState::Uninitialized | Ipv6DadState::Assigned => {
                    panic!("expected address to be tentative; addr={addr:?}")
                }
            };

            match remaining {
                None => {
                    *dad_state = Ipv6DadState::Assigned;
                    core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = true);
                    CC::on_event(
                        bindings_ctx,
                        DadEvent::AddressAssigned {
                            device: device_id.clone(),
                            addr: addr.addr_sub().addr().get(),
                        },
                    );
                    None
                }
                Some(non_zero_remaining) => {
                    *remaining = NonZeroU16::new(non_zero_remaining.get() - 1);

                    // Per RFC 4862 section 5.1,
                    //
                    //   DupAddrDetectTransmits ...
                    //      Autoconfiguration also assumes the presence of the variable
                    //      RetransTimer as defined in [RFC4861]. For autoconfiguration
                    //      purposes, RetransTimer specifies the delay between
                    //      consecutive Neighbor Solicitation transmissions performed
                    //      during Duplicate Address Detection (if
                    //      DupAddrDetectTransmits is greater than 1), as well as the
                    //      time a node waits after sending the last Neighbor
                    //      Solicitation before ending the Duplicate Address Detection
                    //      process.
                    assert_eq!(
                        bindings_ctx.schedule_timer(retrans_timer.get(), timer),
                        None,
                        "Unexpected DAD timer; addr={}, device_id={:?}",
                        addr.addr(),
                        device_id
                    );
                    debug!(
                        "performing DAD for {}; {} tries left",
                        addr.addr(),
                        remaining.map_or(0, NonZeroU16::get)
                    );
                    Some(nonces.evicting_create_and_store_nonce(bindings_ctx.rng()))
                }
            }
        },
    );

    let nonce = match nonce_if_should_send_message {
        None => return,
        Some(nonce) => nonce,
    };

    // Do not include the source link-layer option when the NS
    // message as DAD messages are sent with the unspecified source
    // address which must not hold a source link-layer option.
    //
    // As per RFC 4861 section 4.3,
    //
    //   Possible options:
    //
    //      Source link-layer address
    //           The link-layer address for the sender. MUST NOT be
    //           included when the source IP address is the
    //           unspecified address. Otherwise, on link layers
    //           that have addresses this option MUST be included in
    //           multicast solicitations and SHOULD be included in
    //           unicast solicitations.
    //
    // TODO(https://fxbug.dev/42165912): Either panic or guarantee that this error
    // can't happen statically.
    let dst_ip = addr.addr().addr().to_solicited_node_address();
    let _: Result<(), _> = core_ctx.send_dad_packet(
        bindings_ctx,
        device_id,
        dst_ip,
        NeighborSolicitation::new(addr.addr().addr()),
        NdpNonce::from(&nonce),
    );
}

// TODO(https://fxbug.dev/42077260): Actually support DAD for IPv4.
impl<BC, CC> DadHandler<Ipv4, BC> for CC
where
    CC: IpDeviceAddressIdContext<Ipv4> + DeviceIdContext<AnyDevice>,
{
    const INITIAL_ADDRESS_STATE: IpAddressState = IpAddressState::Assigned;
    fn start_duplicate_address_detection(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr: &Self::AddressId,
    ) {
    }

    fn stop_duplicate_address_detection(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr: &Self::AddressId,
    ) {
    }

    fn handle_incoming_dad_neighbor_solicitation(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr: &Self::AddressId,
        _nonce: Option<NdpNonce<&'_ [u8]>>,
    ) -> DadAddressStateLookupResult {
        unimplemented!()
    }
}

impl<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>> DadHandler<Ipv6, BC> for CC {
    const INITIAL_ADDRESS_STATE: IpAddressState = IpAddressState::Tentative;

    fn start_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    ) {
        do_duplicate_address_detection(self, bindings_ctx, device_id, addr, DoDadVariation::Start)
    }

    fn stop_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    ) {
        self.with_dad_state(
            device_id,
            addr,
            |DadStateRef { state, retrans_timer: _, max_dad_transmits: _ }| {
                let DadAddressStateRef { dad_state, core_ctx } = state;

                let leave_group = match dad_state {
                    Ipv6DadState::Assigned => true,
                    Ipv6DadState::Tentative {
                        dad_transmits_remaining: _,
                        timer,
                        nonces: _,
                        added_extra_transmits_after_detecting_looped_back_ns: _,
                    } => {
                        // Generally we should have a timer installed in the
                        // tentative state, but we could be racing with the
                        // timer firing in bindings so we can't assert that it's
                        // installed here.
                        let _: Option<_> = bindings_ctx.cancel_timer(timer);
                        true
                    }
                    Ipv6DadState::Uninitialized => false,
                };

                // Undo the work we did when starting/performing DAD by putting
                // the address back into a tentative/unassigned state and
                // leaving the solicited node multicast group. We mark the
                // address as tentative/unassigned before leaving the group so
                // that the address is not used as the source for any outgoing
                // MLD message.
                *dad_state = Ipv6DadState::Uninitialized;
                core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = false);
                if leave_group {
                    core_ctx.leave_multicast_group(
                        bindings_ctx,
                        device_id,
                        addr.addr().addr().to_solicited_node_address(),
                    );
                }
            },
        )
    }

    /// Checks if the nonce matches stored nonces in dad state.
    fn handle_incoming_dad_neighbor_solicitation(
        &mut self,
        _bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        nonce: Option<NdpNonce<&'_ [u8]>>,
    ) -> DadAddressStateLookupResult {
        self.with_dad_state(
            device_id,
            addr,
            |DadStateRef { state, retrans_timer: _, max_dad_transmits: _ }| {
                let DadAddressStateRef { dad_state, core_ctx: _ } = state;
                match dad_state {
                    Ipv6DadState::Assigned => DadAddressStateLookupResult::Assigned,
                    Ipv6DadState::Tentative {
                        dad_transmits_remaining,
                        nonces,
                        added_extra_transmits_after_detecting_looped_back_ns,
                        timer: _,
                    } => {
                        let matched_nonce =
                            nonce.is_some_and(|nonce| nonces.contains(nonce.bytes()));
                        if matched_nonce
                            && !core::mem::replace(
                                added_extra_transmits_after_detecting_looped_back_ns,
                                true,
                            )
                        {
                            // Detected a looped-back DAD neighbor solicitation.
                            // Per RFC 7527, we should send MAX_MULTICAST_SOLICIT more DAD probes.
                            *dad_transmits_remaining =
                                Some(DEFAULT_MAX_MULTICAST_SOLICIT.saturating_add(
                                    dad_transmits_remaining.map(NonZero::get).unwrap_or(0),
                                ));
                        }
                        DadAddressStateLookupResult::Tentative { matched_nonce }
                    }

                    Ipv6DadState::Uninitialized => DadAddressStateLookupResult::Uninitialized,
                }
            },
        )
    }
}

impl<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>> HandleableTimer<CC, BC>
    for DadTimerId<CC::WeakDeviceId, CC::WeakAddressId>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, _: BC::UniqueTimerId) {
        let Self { device_id, addr } = self;
        let Some(device_id) = device_id.upgrade() else {
            return;
        };
        let Some(addr_id) = addr.upgrade() else {
            return;
        };
        do_duplicate_address_detection(
            core_ctx,
            bindings_ctx,
            &device_id,
            &addr_id,
            DoDadVariation::Continue,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::hash_map::{Entry, HashMap};
    use alloc::vec::Vec;
    use const_unwrap::const_unwrap_option;
    use core::time::Duration;

    use assert_matches::assert_matches;
    use net_types::ip::{AddrSubnet, IpAddress as _};
    use net_types::Witness as _;
    use netstack3_base::testutil::{
        FakeBindingsCtx, FakeCoreCtx, FakeDeviceId, FakeTimerCtxExt as _, FakeWeakAddressId,
        FakeWeakDeviceId,
    };
    use netstack3_base::{CtxPair, InstantContext as _, SendFrameContext as _, TimerHandler};
    use packet::EmptyBuf;
    use packet_formats::icmp::ndp::Options;
    use test_case::test_case;

    use super::*;
    use crate::internal::device::Ipv6DeviceAddr;

    struct FakeDadAddressContext {
        addr: UnicastAddr<Ipv6Addr>,
        assigned: bool,
        groups: HashMap<MulticastAddr<Ipv6Addr>, usize>,
    }

    type FakeAddressCtxImpl = FakeCoreCtx<FakeDadAddressContext, (), FakeDeviceId>;

    impl DadAddressContext<FakeBindingsCtxImpl> for FakeAddressCtxImpl {
        fn with_address_assigned<O, F: FnOnce(&mut bool) -> O>(
            &mut self,
            &FakeDeviceId: &Self::DeviceId,
            request_addr: &Self::AddressId,
            cb: F,
        ) -> O {
            let FakeDadAddressContext { addr, assigned, .. } = &mut self.state;
            assert_eq!(*request_addr.addr(), *addr);
            cb(assigned)
        }

        fn join_multicast_group(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &Self::DeviceId,
            multicast_addr: MulticastAddr<Ipv6Addr>,
        ) {
            *self.state.groups.entry(multicast_addr).or_default() += 1;
        }

        fn leave_multicast_group(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &Self::DeviceId,
            multicast_addr: MulticastAddr<Ipv6Addr>,
        ) {
            match self.state.groups.entry(multicast_addr) {
                Entry::Vacant(_) => {}
                Entry::Occupied(mut e) => {
                    let v = e.get_mut();
                    const COUNT_BEFORE_REMOVE: usize = 1;
                    if *v == COUNT_BEFORE_REMOVE {
                        assert_eq!(e.remove(), COUNT_BEFORE_REMOVE);
                    } else {
                        *v -= 1
                    }
                }
            }
        }
    }

    struct FakeDadContext {
        state: Ipv6DadState<FakeBindingsCtxImpl>,
        retrans_timer: NonZeroDuration,
        max_dad_transmits: Option<NonZeroU16>,
        address_ctx: FakeAddressCtxImpl,
    }

    #[derive(Debug)]
    struct DadMessageMeta {
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
        nonce: Vec<u8>,
    }

    type TestDadTimerId = DadTimerId<
        FakeWeakDeviceId<FakeDeviceId>,
        FakeWeakAddressId<AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>>,
    >;

    type FakeBindingsCtxImpl = FakeBindingsCtx<TestDadTimerId, DadEvent<FakeDeviceId>, (), ()>;

    type FakeCoreCtxImpl = FakeCoreCtx<FakeDadContext, DadMessageMeta, FakeDeviceId>;

    fn get_address_id(addr: Ipv6Addr) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        AddrSubnet::new(addr, Ipv6Addr::BYTES * 8).unwrap()
    }

    impl CoreTimerContext<TestDadTimerId, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn convert_timer(dispatch_id: TestDadTimerId) -> TestDadTimerId {
            dispatch_id
        }
    }

    impl CoreEventContext<DadEvent<FakeDeviceId>> for FakeCoreCtxImpl {
        type OuterEvent = DadEvent<FakeDeviceId>;
        fn convert_event(event: DadEvent<FakeDeviceId>) -> DadEvent<FakeDeviceId> {
            event
        }
    }

    impl DadContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type DadAddressCtx<'a> = FakeAddressCtxImpl;

        fn with_dad_state<
            O,
            F: FnOnce(DadStateRef<'_, Self::DadAddressCtx<'_>, FakeBindingsCtxImpl>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            request_addr: &Self::AddressId,
            cb: F,
        ) -> O {
            let FakeDadContext { state, retrans_timer, max_dad_transmits, address_ctx } =
                &mut self.state;
            let ctx_addr = address_ctx.state.addr;
            let requested_addr = request_addr.addr().get();
            assert!(
                ctx_addr == requested_addr,
                "invalid address {requested_addr} expected {ctx_addr}"
            );
            cb(DadStateRef {
                state: DadAddressStateRef { dad_state: state, core_ctx: address_ctx },
                retrans_timer,
                max_dad_transmits,
            })
        }

        fn send_dad_packet(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &FakeDeviceId,
            dst_ip: MulticastAddr<Ipv6Addr>,
            message: NeighborSolicitation,
            nonce: NdpNonce<&[u8]>,
        ) -> Result<(), ()> {
            Ok(self
                .send_frame(
                    bindings_ctx,
                    DadMessageMeta { dst_ip, message, nonce: nonce.bytes().to_vec() },
                    EmptyBuf,
                )
                .unwrap())
        }
    }

    const RETRANS_TIMER: NonZeroDuration =
        const_unwrap_option(NonZeroDuration::new(Duration::from_secs(1)));

    const DAD_ADDRESS: UnicastAddr<Ipv6Addr> =
        unsafe { UnicastAddr::new_unchecked(Ipv6Addr::new([0xa, 0, 0, 0, 0, 0, 0, 1])) };

    type FakeCtx = CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl>;

    #[test]
    #[should_panic(expected = "expected address to be tentative")]
    fn panic_non_tentative_address_handle_timer() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::with_state(FakeDadContext {
                state: Ipv6DadState::Assigned,
                retrans_timer: RETRANS_TIMER,
                max_dad_transmits: None,
                address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                    addr: DAD_ADDRESS,
                    assigned: false,
                    groups: HashMap::default(),
                }),
            }));
        TimerHandler::handle_timer(
            &mut core_ctx,
            &mut bindings_ctx,
            dad_timer_id(),
            Default::default(),
        );
    }

    #[test]
    fn dad_disabled() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
                FakeCoreCtxImpl::with_state(FakeDadContext {
                    state: Ipv6DadState::Tentative {
                        dad_transmits_remaining: None,
                        timer: bindings_ctx.new_timer(dad_timer_id()),
                        nonces: Default::default(),
                        added_extra_transmits_after_detecting_looped_back_ns: false,
                    },
                    retrans_timer: RETRANS_TIMER,
                    max_dad_transmits: None,
                    address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                        addr: DAD_ADDRESS,
                        assigned: false,
                        groups: HashMap::default(),
                    }),
                })
            });
        DadHandler::<Ipv6, _>::start_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Assigned);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        assert_eq!(
            bindings_ctx.take_events(),
            &[DadEvent::AddressAssigned { device: FakeDeviceId, addr: DAD_ADDRESS }][..]
        );
    }

    fn dad_timer_id() -> TestDadTimerId {
        DadTimerId {
            addr: FakeWeakAddressId(get_address_id(DAD_ADDRESS.get())),
            device_id: FakeWeakDeviceId(FakeDeviceId),
        }
    }

    fn check_dad(
        core_ctx: &FakeCoreCtxImpl,
        bindings_ctx: &FakeBindingsCtxImpl,
        frames_len: usize,
        dad_transmits_remaining: Option<NonZeroU16>,
        retrans_timer: NonZeroDuration,
    ) {
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        let nonces = assert_matches!(state, Ipv6DadState::Tentative {
            dad_transmits_remaining: got,
            timer: _,
            nonces,
            added_extra_transmits_after_detecting_looped_back_ns: _,
        } => {
            assert_eq!(
                *got,
                dad_transmits_remaining,
                "got dad_transmits_remaining = {got:?}, \
                 want dad_transmits_remaining = {dad_transmits_remaining:?}");
            nonces
        });
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(!*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        let frames = core_ctx.frames();
        assert_eq!(frames.len(), frames_len, "frames = {:?}", frames);
        let (DadMessageMeta { dst_ip, message, nonce }, frame) =
            frames.last().expect("should have transmitted a frame");

        assert_eq!(*dst_ip, DAD_ADDRESS.to_solicited_node_address());
        assert_eq!(*message, NeighborSolicitation::new(DAD_ADDRESS.get()));
        assert!(nonces.contains(nonce), "should have stored nonce");

        let options = Options::parse(&frame[..]).expect("parse NDP options");
        assert_eq!(options.iter().count(), 0);
        bindings_ctx
            .timers
            .assert_timers_installed([(dad_timer_id(), bindings_ctx.now() + retrans_timer.get())]);
    }

    #[test]
    fn perform_dad() {
        const DAD_TRANSMITS_REQUIRED: u16 = 5;
        const RETRANS_TIMER: NonZeroDuration =
            const_unwrap_option(NonZeroDuration::new(Duration::from_secs(1)));

        let mut ctx = FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeDadContext {
                state: Ipv6DadState::Tentative {
                    dad_transmits_remaining: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                    timer: bindings_ctx.new_timer(dad_timer_id()),
                    nonces: Default::default(),
                    added_extra_transmits_after_detecting_looped_back_ns: false,
                },
                retrans_timer: RETRANS_TIMER,
                max_dad_transmits: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                    addr: DAD_ADDRESS,
                    assigned: false,
                    groups: HashMap::default(),
                }),
            })
        });
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        DadHandler::<Ipv6, _>::start_duplicate_address_detection(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );

        for count in 0..=(DAD_TRANSMITS_REQUIRED - 1) {
            check_dad(
                core_ctx,
                bindings_ctx,
                usize::from(count + 1),
                NonZeroU16::new(DAD_TRANSMITS_REQUIRED - count - 1),
                RETRANS_TIMER,
            );
            assert_eq!(bindings_ctx.trigger_next_timer(core_ctx), Some(dad_timer_id()));
        }
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Assigned);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        assert_eq!(
            bindings_ctx.take_events(),
            &[DadEvent::AddressAssigned { device: FakeDeviceId, addr: DAD_ADDRESS }][..]
        );
    }

    #[test]
    fn stop_dad() {
        const DAD_TRANSMITS_REQUIRED: u16 = 2;
        const RETRANS_TIMER: NonZeroDuration =
            const_unwrap_option(NonZeroDuration::new(Duration::from_secs(2)));

        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
                FakeCoreCtxImpl::with_state(FakeDadContext {
                    state: Ipv6DadState::Tentative {
                        dad_transmits_remaining: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                        timer: bindings_ctx.new_timer(dad_timer_id()),
                        nonces: Default::default(),
                        added_extra_transmits_after_detecting_looped_back_ns: false,
                    },
                    retrans_timer: RETRANS_TIMER,
                    max_dad_transmits: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                    address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                        addr: DAD_ADDRESS,
                        assigned: false,
                        groups: HashMap::default(),
                    }),
                })
            });
        DadHandler::<Ipv6, _>::start_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        check_dad(
            &core_ctx,
            &bindings_ctx,
            1,
            NonZeroU16::new(DAD_TRANSMITS_REQUIRED - 1),
            RETRANS_TIMER,
        );

        DadHandler::<Ipv6, _>::stop_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        bindings_ctx.timers.assert_no_timers_installed();
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Uninitialized);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(!*assigned);
        assert_eq!(groups, &HashMap::new());
    }

    #[test_case(true, None ; "assigned with no incoming nonce")]
    #[test_case(true, Some([1u8; MIN_NONCE_LENGTH]) ; "assigned with incoming nonce")]
    #[test_case(false, None ; "uninitialized with no incoming nonce")]
    #[test_case(false, Some([1u8; MIN_NONCE_LENGTH]) ; "uninitialized with incoming nonce")]
    fn handle_incoming_dad_neighbor_solicitation_while_not_tentative(
        assigned: bool,
        nonce: Option<[u8; MIN_NONCE_LENGTH]>,
    ) {
        const MAX_DAD_TRANSMITS: u16 = 1;
        const RETRANS_TIMER: NonZeroDuration =
            const_unwrap_option(NonZeroDuration::new(Duration::from_secs(1)));

        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtxImpl::with_state(FakeDadContext {
            state: if assigned { Ipv6DadState::Assigned } else { Ipv6DadState::Uninitialized },
            retrans_timer: RETRANS_TIMER,
            max_dad_transmits: NonZeroU16::new(MAX_DAD_TRANSMITS),
            address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                addr: DAD_ADDRESS,
                assigned: false,
                groups: HashMap::default(),
            }),
        }));
        let addr = get_address_id(DAD_ADDRESS.get());

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;

        let want_lookup_result = if assigned {
            DadAddressStateLookupResult::Assigned
        } else {
            DadAddressStateLookupResult::Uninitialized
        };

        assert_eq!(
            DadHandler::<Ipv6, _>::handle_incoming_dad_neighbor_solicitation(
                core_ctx,
                bindings_ctx,
                &FakeDeviceId,
                &addr,
                nonce.as_ref().map(NdpNonce::from),
            ),
            want_lookup_result
        );
    }

    #[test_case(true ; "discards looped back NS")]
    #[test_case(false ; "acts on non-looped-back NS")]
    fn handle_incoming_dad_neighbor_solicitation_during_tentative(looped_back: bool) {
        const DAD_TRANSMITS_REQUIRED: u16 = 1;
        const RETRANS_TIMER: NonZeroDuration =
            const_unwrap_option(NonZeroDuration::new(Duration::from_secs(1)));

        let mut ctx = FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeDadContext {
                state: Ipv6DadState::Tentative {
                    dad_transmits_remaining: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                    timer: bindings_ctx.new_timer(dad_timer_id()),
                    nonces: Default::default(),
                    added_extra_transmits_after_detecting_looped_back_ns: false,
                },
                retrans_timer: RETRANS_TIMER,
                max_dad_transmits: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                    addr: DAD_ADDRESS,
                    assigned: false,
                    groups: HashMap::default(),
                }),
            })
        });
        let addr = get_address_id(DAD_ADDRESS.get());

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        DadHandler::<Ipv6, _>::start_duplicate_address_detection(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            &addr,
        );

        check_dad(core_ctx, bindings_ctx, 1, None, RETRANS_TIMER);

        let sent_nonce: [u8; MIN_NONCE_LENGTH] = {
            let (DadMessageMeta { dst_ip: _, message: _, nonce }, _frame) =
                core_ctx.frames().last().expect("should have transmitted a frame");
            nonce.clone().try_into().expect("should be nonce of MIN_NONCE_LENGTH")
        };

        let alternative_nonce = {
            let mut nonce = sent_nonce.clone();
            nonce[0] = nonce[0].wrapping_add(1);
            nonce
        };

        let incoming_nonce =
            NdpNonce::from(if looped_back { &sent_nonce } else { &alternative_nonce });

        let matched_nonce = assert_matches!(
            DadHandler::<Ipv6, _>::handle_incoming_dad_neighbor_solicitation(
                core_ctx,
                bindings_ctx,
                &FakeDeviceId,
                &addr,
                Some(incoming_nonce),
            ),
            DadAddressStateLookupResult::Tentative { matched_nonce } => matched_nonce
        );

        assert_eq!(matched_nonce, looped_back);

        let frames_len_before_extra_transmits = core_ctx.frames().len();
        assert_eq!(frames_len_before_extra_transmits, 1);

        let extra_dad_transmits_required =
            NonZero::new(if looped_back { DEFAULT_MAX_MULTICAST_SOLICIT.get() } else { 0 });

        let (dad_transmits_remaining, added_extra_transmits_after_detecting_looped_back_ns) = assert_matches!(
            &core_ctx.state.state,
            Ipv6DadState::Tentative {
                dad_transmits_remaining,
                timer: _,
                nonces: _,
                added_extra_transmits_after_detecting_looped_back_ns
            } => (dad_transmits_remaining, added_extra_transmits_after_detecting_looped_back_ns),
            "DAD state should be Tentative"
        );

        assert_eq!(dad_transmits_remaining, &extra_dad_transmits_required);
        assert_eq!(added_extra_transmits_after_detecting_looped_back_ns, &looped_back);

        let extra_dad_transmits_required =
            extra_dad_transmits_required.map(|n| n.get()).unwrap_or(0);

        // The retransmit timer should have been kicked when we observed the matching nonce.
        assert_eq!(bindings_ctx.trigger_next_timer(core_ctx), Some(dad_timer_id()));

        // Even though we originally required only 1 DAD transmit, MAX_MULTICAST_SOLICIT more
        // should be required as a result of the looped back solicitation.
        for count in 0..extra_dad_transmits_required {
            check_dad(
                core_ctx,
                bindings_ctx,
                usize::from(count) + frames_len_before_extra_transmits + 1,
                NonZeroU16::new(extra_dad_transmits_required - count - 1),
                RETRANS_TIMER,
            );
            assert_eq!(bindings_ctx.trigger_next_timer(core_ctx), Some(dad_timer_id()));
        }
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Assigned);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        assert_eq!(
            bindings_ctx.take_events(),
            &[DadEvent::AddressAssigned { device: FakeDeviceId, addr: DAD_ADDRESS }][..]
        );
    }
}
