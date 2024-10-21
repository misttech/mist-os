// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use derivative::Derivative;
use fidl::endpoints::ControlHandle;
use fidl_fuchsia_net::Subnet;
use fnet_masquerade::Error;
use fuchsia_async::DurationExt as _;
use futures::stream::LocalBoxStream;
use futures::{future, StreamExt as _, TryStreamExt as _};
use net_declare::fidl_subnet;
use tracing::{error, warn};
use {
    fidl_fuchsia_net_filter_deprecated as fnet_filter_deprecated,
    fidl_fuchsia_net_masquerade as fnet_masquerade,
};

use crate::filter::FilterEnabledState;
use crate::{InterfaceId, InterfaceState};

const UNSPECIFIED_SUBNET: Subnet = fidl_subnet!("0.0.0.0/0");

#[derive(Derivative)]
#[derivative(Debug)]
pub(super) enum Event {
    FactoryRequestStream(#[derivative(Debug = "ignore")] fnet_masquerade::FactoryRequestStream),
    FactoryRequest(fnet_masquerade::FactoryRequest),
    ControlRequest(ValidatedConfig, fnet_masquerade::ControlRequest),
}

pub(super) type EventStream = LocalBoxStream<'static, Result<Event, fidl::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ValidatedConfig {
    /// The network to be masqueraded.
    pub src_subnet: fidl_fuchsia_net::Subnet,
    /// The interface through which to masquerade.
    pub output_interface: InterfaceId,
}

impl TryFrom<fnet_masquerade::ControlConfig> for ValidatedConfig {
    type Error = fnet_masquerade::Error;

    fn try_from(
        fnet_masquerade::ControlConfig { src_subnet, output_interface }: fnet_masquerade::ControlConfig,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            src_subnet,
            output_interface: InterfaceId::new(output_interface).ok_or(Error::InvalidArguments)?,
        })
    }
}

#[derive(Debug, Clone)]
struct MasqueradeState {
    active: bool,
    control: Option<fnet_masquerade::ControlControlHandle>,
}

impl MasqueradeState {
    fn new(control: Option<fnet_masquerade::ControlControlHandle>) -> Self {
        Self { active: false, control }
    }
}

pub(super) struct Masquerade<Filter = fnet_filter_deprecated::FilterProxy> {
    filter: Filter,
    active_controllers: HashMap<ValidatedConfig, MasqueradeState>,
}

/// Updates the interface enabled state to acknowledge the change in masquerade
/// configuration.
///
/// Note: It is incorrect to call this function if no change has occurred.
async fn update_interface<Filter: fnet_filter_deprecated::FilterProxyInterface>(
    filter: &Filter,
    interface: InterfaceId,
    enabled: bool,
    filter_enabled_state: &mut FilterEnabledState,
    interface_states: &HashMap<InterfaceId, InterfaceState>,
) -> Result<(), Error> {
    if enabled {
        filter_enabled_state.increment_masquerade_count_on_interface(interface);
    } else {
        filter_enabled_state.decrement_masquerade_count_on_interface(interface);
    }
    if let Err(e) = filter_enabled_state
        .maybe_update_deprecated(
            interface_states.get(&interface).map(|is| is.device_class.into()),
            interface,
            filter,
        )
        .await
    {
        match e {
            fnet_filter_deprecated::EnableDisableInterfaceError::NotFound => {
                error!("specified input_interface not found: {interface}");
                return Err(Error::NotFound);
            }
        }
    }

    Ok(())
}

impl<Filter: fnet_filter_deprecated::FilterProxyInterface> Masquerade<Filter> {
    pub fn new(filter: Filter) -> Self {
        Self { filter, active_controllers: HashMap::new() }
    }

    async fn set_enabled(
        &mut self,
        config: ValidatedConfig,
        enabled: bool,
        filter_enabled_state: &mut FilterEnabledState,
        interface_states: &HashMap<InterfaceId, InterfaceState>,
    ) -> Result<bool, Error> {
        let state =
            self.active_controllers.get_mut(&config).ok_or_else(|| Error::InvalidArguments)?;
        if state.active == enabled {
            // The current state is already the desired state; short circuit.
            // This prevents calling `update_interface` in the no-change case.
            return Ok(state.active);
        }

        let ValidatedConfig { src_subnet, output_interface } = config;
        let outgoing_nic = output_interface.get();
        update_interface(
            &self.filter,
            output_interface,
            enabled,
            filter_enabled_state,
            interface_states,
        )
        .await?;

        for _ in 0..crate::filter::FILTER_CAS_RETRY_MAX {
            let (mut rules, generation) =
                self.filter.get_nat_rules().await.expect("call to GetNatRules failed");

            if enabled {
                if rules.iter().any(
                    |fnet_filter_deprecated::Nat {
                         src_subnet: old_src_subnet,
                         outgoing_nic: old_outgoing_nic,
                         proto,
                     }| {
                        *old_src_subnet == src_subnet
                            && *old_outgoing_nic == outgoing_nic
                            && *proto == fnet_filter_deprecated::SocketProtocol::Any
                    },
                ) {
                    return Err(Error::AlreadyExists);
                }
                rules.push(fnet_filter_deprecated::Nat {
                    proto: fnet_filter_deprecated::SocketProtocol::Any,
                    src_subnet,
                    outgoing_nic,
                });
            } else {
                rules.retain(
                    |fnet_filter_deprecated::Nat {
                         src_subnet: old_src_subnet,
                         outgoing_nic: old_outgoing_nic,
                         proto,
                     }| {
                        !(*proto == fnet_filter_deprecated::SocketProtocol::Any
                            && *old_src_subnet == src_subnet
                            && *old_outgoing_nic == outgoing_nic)
                    },
                );
            }

            match self
                .filter
                .update_nat_rules(&rules, generation)
                .await
                .expect("call to UpdateNatRules failed")
            {
                Ok(()) => {
                    let was_enabled = state.active;
                    state.active = enabled;
                    return Ok(was_enabled);
                }
                Err(fnet_filter_deprecated::FilterUpdateNatRulesError::GenerationMismatch) => {
                    // We need to try again.
                    fuchsia_async::Timer::new(
                        zx::MonotonicDuration::from_millis(
                            crate::filter::FILTER_CAS_RETRY_INTERVAL_MILLIS,
                        )
                        .after_now(),
                    )
                    .await;
                }
                Err(fnet_filter_deprecated::FilterUpdateNatRulesError::BadRule) => {
                    panic!("Generated Nat rule was invalid. This should never happen: {rules:?}");
                }
            }
        }

        error!("Failed to set new Nat rule");
        Err(Error::RetryExceeded)
    }

    /// Attempts to create a new fuchsia_net_masquerade/Control connection.
    ///
    /// On error, returns the original control handle back so that the caller
    /// may terminate the connection.
    fn create_control(
        &mut self,
        config: ValidatedConfig,
        control: Option<fnet_masquerade::ControlControlHandle>,
    ) -> Result<(), (Error, Option<fnet_masquerade::ControlControlHandle>)> {
        if config.src_subnet == UNSPECIFIED_SUBNET {
            return Err((Error::Unsupported, control));
        }

        match self.active_controllers.entry(config) {
            std::collections::hash_map::Entry::Vacant(e) => {
                // No need to modify the just-added state.
                let _: &mut MasqueradeState = e.insert(MasqueradeState::new(control));
                Ok(())
            }
            // TODO(https://fxbug.dev/374287551): At the moment, new controllers
            // are rejected if their configuration exactly matches an existing
            // controller. However, it would be preferable to also reject
            // controllers that specify an overlapping configuration. E.g. a
            // subnet that overlaps with an existing subnet on the same
            // interface.
            std::collections::hash_map::Entry::Occupied(_) => Err((Error::AlreadyExists, control)),
        }
    }

    pub async fn handle_event<'a>(
        &mut self,
        event: Event,
        events: &mut futures::stream::SelectAll<EventStream>,
        filter_enabled_state: &mut FilterEnabledState,
        interface_states: &HashMap<InterfaceId, InterfaceState>,
    ) {
        match event {
            Event::FactoryRequestStream(stream) => events.push(
                stream.try_filter_map(|r| future::ok(Some(Event::FactoryRequest(r)))).boxed(),
            ),
            Event::FactoryRequest(fnet_masquerade::FactoryRequest::Create {
                config,
                control,
                responder,
            }) => {
                let (stream, control) = control
                    .into_stream_and_control_handle()
                    .expect("convert server end into stream");
                let config = match ValidatedConfig::try_from(config) {
                    Ok(config) => config,
                    Err(e) => {
                        control.respond_and_maybe_shutdown(Err(e), |r| {
                            let _: Result<(), fidl::Error> = responder.send(r);
                            // N.B. we always return Ok here because we don't
                            // want to shut down the Control handle if replying
                            // to the Factory request fails.
                            Ok(())
                        });
                        return;
                    }
                };
                match self.create_control(config, Some(control)) {
                    Ok(()) => {
                        if let Err(e) = responder.send(Ok(())) {
                            error!("failed to notify control of successful creation: {e:?}");
                        }
                        events.push(
                            stream
                                .try_filter_map(move |r| {
                                    future::ok(Some(Event::ControlRequest(config, r)))
                                })
                                .boxed(),
                        );
                    }
                    Err((e, control)) => {
                        warn!("failed to create control: {e:?}");
                        control.respond_and_maybe_shutdown(Err(e), |r| responder.send(r));
                    }
                }
            }
            Event::ControlRequest(
                config,
                fnet_masquerade::ControlRequest::SetEnabled { enabled, responder },
            ) => {
                let response =
                    self.set_enabled(config, enabled, filter_enabled_state, interface_states).await;
                let state = self
                    .active_controllers
                    .get_mut(&config)
                    .expect("no active_controller for the given interface");
                state.respond_and_maybe_shutdown(response, |r| responder.send(r));
            }
        }
    }
}

trait RespondAndMaybeShutdown {
    fn respond_and_maybe_shutdown<T: Clone, Sender>(
        &self,
        response: Result<T, fnet_masquerade::Error>,
        sender: Sender,
    ) where
        Sender: FnOnce(Result<T, fnet_masquerade::Error>) -> Result<(), fidl::Error>;
}

impl RespondAndMaybeShutdown for Option<fnet_masquerade::ControlControlHandle> {
    fn respond_and_maybe_shutdown<T: Clone, Sender>(
        &self,
        response: Result<T, fnet_masquerade::Error>,
        sender: Sender,
    ) where
        Sender: FnOnce(Result<T, fnet_masquerade::Error>) -> Result<(), fidl::Error>,
    {
        if let Some(h) = self {
            h.respond_and_maybe_shutdown(response, sender);
        }
    }
}

fn to_epitaph(e: Error) -> fidl::Status {
    match e {
        Error::Unsupported => fidl::Status::NOT_SUPPORTED,
        Error::InvalidArguments => fidl::Status::INVALID_ARGS,
        Error::NotFound => fidl::Status::NOT_FOUND,
        Error::AlreadyExists => fidl::Status::ALREADY_BOUND,
        Error::BadRule => fidl::Status::BAD_PATH,
        Error::RetryExceeded => fidl::Status::TIMED_OUT,
        e => panic!("Unhandled error {e:?}"),
    }
}

impl RespondAndMaybeShutdown for fnet_masquerade::ControlControlHandle {
    fn respond_and_maybe_shutdown<T: Clone, Sender>(
        &self,
        response: Result<T, fnet_masquerade::Error>,
        sender: Sender,
    ) where
        Sender: FnOnce(Result<T, fnet_masquerade::Error>) -> Result<(), fidl::Error>,
    {
        // This is not a permanent error, and should not cause a shutdown.
        if let Err(err) = sender(response.clone()) {
            error!("Shutting down due to fidl error: {err:?}");
            self.shutdown_with_epitaph(fidl::Status::INTERNAL);
            return;
        }
        if let Err(e) = response {
            match e {
                Error::RetryExceeded => {
                    // This is not a permanent error, and should not cause a shutdown.
                }
                e => {
                    warn!("Shutting down due to permanent error: {e:?}");
                    self.shutdown_with_epitaph(to_epitaph(e));
                }
            }
        }
    }
}

impl RespondAndMaybeShutdown for MasqueradeState {
    fn respond_and_maybe_shutdown<T: Clone, Sender>(
        &self,
        response: Result<T, fnet_masquerade::Error>,
        sender: Sender,
    ) where
        Sender: FnOnce(Result<T, fnet_masquerade::Error>) -> Result<(), fidl::Error>,
    {
        self.control.respond_and_maybe_shutdown(response, sender)
    }
}

#[cfg(test)]
pub mod test {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use test_case::test_case;

    use super::*;

    impl ValidatedConfig {
        const fn new(src_subnet: fidl_fuchsia_net::Subnet, output_interface: InterfaceId) -> Self {
            Self { src_subnet, output_interface }
        }
    }

    #[derive(Default)]
    struct MockFilterState {
        active_interfaces: HashSet<u64>,
        nat_rules: Vec<fnet_filter_deprecated::Nat>,
        nat_rules_generation: u32,
        fail_generations: i32,
    }

    const VALID_OUTPUT_INTERFACE: InterfaceId = const_unwrap_option(InterfaceId::new(11));
    const NON_EXISTENT_INTERFACE: InterfaceId = const_unwrap_option(InterfaceId::new(1005));

    const VALID_SUBNET: Subnet = fidl_subnet!("192.0.2.0/24");

    const DEFAULT_CONFIG: ValidatedConfig =
        ValidatedConfig::new(VALID_SUBNET, VALID_OUTPUT_INTERFACE);

    #[derive(Default)]
    struct MockFilter {
        state: Arc<Mutex<MockFilterState>>,
    }

    impl fnet_filter_deprecated::FilterProxyInterface for MockFilter {
        type EnableInterfaceResponseFut =
            future::Ready<Result<fnet_filter_deprecated::FilterEnableInterfaceResult, fidl::Error>>;

        fn enable_interface(&self, id: u64) -> Self::EnableInterfaceResponseFut {
            if id == NON_EXISTENT_INTERFACE.get() {
                future::ok(Err(fnet_filter_deprecated::EnableDisableInterfaceError::NotFound))
            } else {
                let _: bool =
                    self.state.lock().expect("lock poisoned").active_interfaces.insert(id);
                future::ok(Ok(()))
            }
        }

        type DisableInterfaceResponseFut = future::Ready<
            Result<fnet_filter_deprecated::FilterDisableInterfaceResult, fidl::Error>,
        >;

        fn disable_interface(&self, id: u64) -> Self::DisableInterfaceResponseFut {
            if id == NON_EXISTENT_INTERFACE.get() {
                future::ok(Err(fnet_filter_deprecated::EnableDisableInterfaceError::NotFound))
            } else {
                let _: bool =
                    self.state.lock().expect("lock poisoned").active_interfaces.remove(&id);
                future::ok(Ok(()))
            }
        }

        type GetNatRulesResponseFut =
            future::Ready<Result<(Vec<fnet_filter_deprecated::Nat>, u32), fidl::Error>>;

        fn get_nat_rules(&self) -> Self::GetNatRulesResponseFut {
            let mut state = self.state.lock().expect("lock poisoned");

            let result = future::ok((state.nat_rules.clone(), state.nat_rules_generation));
            if state.fail_generations > 0 {
                state.nat_rules_generation += 1;
                state.fail_generations -= 1;
            }
            result
        }

        type UpdateNatRulesResponseFut =
            future::Ready<Result<fnet_filter_deprecated::FilterUpdateNatRulesResult, fidl::Error>>;

        fn update_nat_rules(
            &self,
            rules: &[fnet_filter_deprecated::Nat],
            generation: u32,
        ) -> Self::UpdateNatRulesResponseFut {
            let mut state = self.state.lock().expect("lock poisoned");
            if state.nat_rules_generation != generation {
                future::ok(Err(
                    fnet_filter_deprecated::FilterUpdateNatRulesError::GenerationMismatch,
                ))
            } else {
                let new_nat_rules: Vec<fnet_filter_deprecated::Nat> =
                    rules.iter().map(|r| r.clone()).collect();
                state.nat_rules = new_nat_rules;
                state.nat_rules_generation += 1;
                future::ok(Ok(()))
            }
        }

        type GetRulesResponseFut =
            future::Ready<Result<(Vec<fnet_filter_deprecated::Rule>, u32), fidl::Error>>;
        fn get_rules(&self) -> Self::GetRulesResponseFut {
            unreachable!();
        }
        type UpdateRulesResponseFut =
            future::Ready<Result<fnet_filter_deprecated::FilterUpdateRulesResult, fidl::Error>>;
        fn update_rules(
            &self,
            _: &[fnet_filter_deprecated::Rule],
            _: u32,
        ) -> Self::UpdateRulesResponseFut {
            unreachable!();
        }
        type GetRdrRulesResponseFut =
            future::Ready<Result<(Vec<fnet_filter_deprecated::Rdr>, u32), fidl::Error>>;
        fn get_rdr_rules(&self) -> Self::GetRdrRulesResponseFut {
            unreachable!();
        }
        type UpdateRdrRulesResponseFut =
            future::Ready<Result<fnet_filter_deprecated::FilterUpdateRdrRulesResult, fidl::Error>>;
        fn update_rdr_rules(
            &self,
            _: &[fnet_filter_deprecated::Rdr],
            _: u32,
        ) -> Self::UpdateRdrRulesResponseFut {
            unreachable!();
        }
        type CheckPresenceResponseFut = future::Ready<Result<(), fidl::Error>>;
        fn check_presence(&self) -> Self::CheckPresenceResponseFut {
            unreachable!();
        }
    }

    #[fuchsia::test]
    async fn enable_disable_masquerade() {
        let filter = MockFilter::default();
        let mut filter_enabled_state = FilterEnabledState::default();
        let interface_states = HashMap::new();
        let state = filter.state.clone();
        let mut masq = Masquerade::new(filter);
        assert_matches!(masq.create_control(DEFAULT_CONFIG, None), Ok(()));
        assert_matches!(
            masq.set_enabled(DEFAULT_CONFIG, true, &mut filter_enabled_state, &interface_states)
                .await,
            Ok(false)
        );
        {
            let s = state.lock().expect("lock poison");
            assert_eq!(s.active_interfaces.len(), 1);
            assert!(s.active_interfaces.contains(&VALID_OUTPUT_INTERFACE.get()));

            assert_eq!(s.nat_rules.len(), 1);
            assert_eq!(s.nat_rules[0].outgoing_nic, VALID_OUTPUT_INTERFACE.get());
        }
        assert_matches!(
            masq.set_enabled(DEFAULT_CONFIG, false, &mut filter_enabled_state, &interface_states)
                .await,
            Ok(true)
        );
        {
            let s = state.lock().expect("lock poison");
            assert_eq!(s.active_interfaces.len(), 0);
            assert_eq!(s.nat_rules.len(), 0);
        }
    }

    #[test_case(
        DEFAULT_CONFIG,
        Some(crate::filter::FILTER_CAS_RETRY_MAX),
        Ok(()),
        Err(Error::RetryExceeded),
        Ok(false);
        "repeated generation mismatch"
    )]
    #[test_case(
        ValidatedConfig {
            output_interface: NON_EXISTENT_INTERFACE,
            ..DEFAULT_CONFIG
        },
        None,
        Ok(()),
        Err(Error::NotFound),
        Err(Error::NotFound);
        "non existent interface"
    )]
    #[test_case(
        ValidatedConfig {
            src_subnet: UNSPECIFIED_SUBNET,
            ..DEFAULT_CONFIG
        },
        None,
        Err(Error::Unsupported),
        Err(Error::InvalidArguments),
        Err(Error::InvalidArguments);
        "invalid subnet"
    )]
    #[fuchsia::test]
    async fn masquerade(
        config: ValidatedConfig,
        fail_generations: Option<i32>,
        create_control_response: Result<(), Error>,
        first_response: Result<bool, Error>,
        second_response: Result<bool, Error>,
    ) {
        let filter = MockFilter::default();
        let mut filter_enabled_state = FilterEnabledState::default();
        let interface_states = HashMap::new();
        if let Some(generations) = fail_generations {
            filter.state.lock().expect("lock poison").fail_generations = generations;
        }
        let mut masq = Masquerade::new(filter);
        pretty_assertions::assert_eq!(
            masq.create_control(config.clone(), None).map_err(|(e, _control)| e),
            create_control_response
        );
        pretty_assertions::assert_eq!(
            masq.set_enabled(config, true, &mut filter_enabled_state, &interface_states).await,
            first_response
        );
        pretty_assertions::assert_eq!(
            masq.set_enabled(config, true, &mut filter_enabled_state, &interface_states).await,
            second_response
        );
    }
}
