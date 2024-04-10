// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    num::NonZeroU64,
};

use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_deprecated as fnet_filter_deprecated;
use fidl_fuchsia_net_filter_ext::{
    self as fnet_filter_ext, Change, Domain, InstalledIpRoutine, InterfaceMatcher, IpHook,
    Matchers, Namespace, NamespaceId, Resource, ResourceId, Routine, RoutineId, RoutineType,
    RuleId,
};
use fuchsia_async::DurationExt as _;
use fuchsia_zircon::DurationNum as _;

use anyhow::{bail, Context as _};
use tracing::{error, info};

use crate::{exit_with_fidl_error, FilterConfig, InterfaceType};

// A container to dispatch filtering functions depending on the
// filtering API present.
pub(crate) enum FilterControl {
    Deprecated(fnet_filter_deprecated::FilterProxy),
    Current(FilterState),
}

impl FilterControl {
    // Determine whether to use the fuchsia.net.filter.deprecated API or the
    // fuchsia.net.filter API. When the deprecated API is present and active,
    // we should use it.
    pub(super) async fn new(
        deprecated_proxy: Option<fnet_filter_deprecated::FilterProxy>,
        current_proxy: Option<fnet_filter::ControlProxy>,
    ) -> Result<Self, anyhow::Error> {
        if let Some(proxy) = deprecated_proxy {
            if probe_for_presence(&proxy).await {
                return Ok(FilterControl::Deprecated(proxy));
            }
        }

        if let Some(proxy) = current_proxy {
            let controller_id = fnet_filter_ext::ControllerId(String::from("netcfg"));
            let filter = FilterControl::Current(FilterState {
                controller: fnet_filter_ext::Controller::new(&proxy, &controller_id)
                    .await
                    .context("could not create controller from filter proxy")?,
                uninstalled_ip_routines: filter_routines(false /* installed */),
                installed_ip_routines: filter_routines(true /* installed */),
                current_installed_rule_index: 0,
            });
            return Ok(filter);
        }

        Err(anyhow::anyhow!("no filtering proxy available!"))
    }

    /// Updates the initial network filter configuration using either
    /// fuchsia.net.filter.deprecated or fuchsia.net.filter.
    pub(super) async fn update_filters(
        &mut self,
        config: FilterConfig,
    ) -> Result<(), anyhow::Error> {
        match self {
            FilterControl::Deprecated(proxy) => update_filters_deprecated(proxy, config).await,
            FilterControl::Current(state) => state.update_filters_current(config).await,
        }
    }
}

pub(super) struct FilterState {
    controller: fnet_filter_ext::Controller,
    uninstalled_ip_routines: netfilter::parser::FilterRoutines,
    installed_ip_routines: netfilter::parser::FilterRoutines,
    current_installed_rule_index: u32,
    // TODO(https://fxbug.dev/331469354): Add NAT routines when this
    // functionality has been added to fuchsia.net.filter.
}

impl FilterState {
    // Commit the initial filter state using fuchsia.net.filter.
    async fn update_filters_current(&mut self, config: FilterConfig) -> Result<(), anyhow::Error> {
        let changes = generate_initial_filter_changes(
            &self.uninstalled_ip_routines,
            &self.installed_ip_routines,
            config,
        )?;

        self.controller
            .push_changes(changes)
            .await
            .context("failed to push changes to filter controller")?;

        self.controller.commit().await.context("failed to commit changes to filter controller")?;
        info!("initial filter configuration has been committed successfully");
        Ok(())
    }
}

// Netcfg's `FilterRoutines` to maintain the same namespace
// and routine for each of the filter `Rule`s across installed
// and uninstalled routines.
fn filter_routines(installed: bool) -> netfilter::parser::FilterRoutines {
    let suffix = if !installed { "_uninstalled" } else { "" };
    netfilter::parser::FilterRoutines {
        local_ingress: Some(RoutineId {
            namespace: namespace_id(),
            name: format!("local_ingress{suffix}"),
        }),
        local_egress: Some(RoutineId {
            namespace: namespace_id(),
            name: format!("local_egress{suffix}"),
        }),
    }
}

fn namespace_id() -> NamespaceId {
    NamespaceId(String::from("netcfg"))
}

pub(super) async fn probe_for_presence(filter: &fnet_filter_deprecated::FilterProxy) -> bool {
    match filter.check_presence().await {
        Ok(()) => true,
        Err(fidl::Error::ClientChannelClosed { status: _, protocol_name: _ }) => false,
        Err(e) => panic!("unexpected error while probing: {e}"),
    }
}

// Create a set of `fnet_filter_ext::Change`s that, when used with
// `fnet_filter_ext::Controller`, will establish the initial filtering
// state for the `netcfg` namespace.
fn generate_initial_filter_changes(
    uninstalled_ip_routines: &netfilter::parser::FilterRoutines,
    installed_ip_routines: &netfilter::parser::FilterRoutines,
    config: FilterConfig,
) -> Result<Vec<Change>, anyhow::Error> {
    let namespace = Change::Create(Resource::Namespace(Namespace {
        id: NamespaceId(String::from("netcfg")),
        domain: Domain::AllIp,
    }));
    let mut changes = vec![namespace];

    // Create uninstalled `Routine`s that the installed `Routine`s can use to
    // `Jump` to `Rule`s. There must be a separate uninstalled `Routine` for
    // each `IpHook` so that there are not issues with a `Rule` containing
    // a matcher that is not allowed in the installed `Routine`'s hook.
    // E.g., A `Rule` in an installed ingress hook `Routine` that `Jump`s to a
    // `Routine` with a `Rule` that specifies an out_interface matcher is not
    // permitted.
    let netfilter::parser::FilterRoutines { local_ingress, local_egress } = uninstalled_ip_routines;
    let uninstalled_local_ingress =
        local_ingress.clone().map(|id| Routine { id, routine_type: RoutineType::Ip(None) });
    let uninstalled_local_egress =
        local_egress.clone().map(|id| Routine { id, routine_type: RoutineType::Ip(None) });

    // Push installed routines so that netcfg can install `Jump` rules
    // at interface installation time that are rooted in these routines.
    fn installed_routine_from_id(id: RoutineId, hook: IpHook) -> Routine {
        Routine {
            id: id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine { hook, priority: 0i32 })),
        }
    }
    let netfilter::parser::FilterRoutines { local_ingress, local_egress } = installed_ip_routines;
    let local_ingress =
        local_ingress.clone().map(|id| installed_routine_from_id(id, IpHook::LocalIngress));
    let local_egress =
        local_egress.clone().map(|id| installed_routine_from_id(id, IpHook::LocalEgress));

    let routine_changes =
        vec![uninstalled_local_ingress, local_ingress, uninstalled_local_egress, local_egress]
            .into_iter()
            .filter_map(|routine| routine)
            .map(|routine| Change::Create(Resource::Routine(routine)));
    changes.extend(routine_changes);

    // TODO(https://fxbug.dev/331469354): Handle NAT and NAT RDR rules when supported
    // by netfilter and filtering library
    let FilterConfig { rules, nat_rules: _, rdr_rules: _ } = config;
    if !rules.is_empty() {
        // Only insert the rules from the config into the uninstalled routine.
        // The rules inserted in the installed routines will be intended for
        // redirection to the uninstalled routines.
        let rules =
            netfilter::parser::parse_str_to_rules(&rules.join(""), &uninstalled_ip_routines)
                .context("error parsing filter rules")?;
        let rule_changes = rules.into_iter().map(|rule| Change::Create(Resource::Rule(rule)));
        changes.extend(rule_changes);
    }

    Ok(changes)
}

// Create a list of `fnet_filter_ext::Rule`s that, when used with
// `fnet_filter_ext::Controller`, will `Jump` on each available
// `IpHook` to the corresponding uninstalled routine for
// that `IpHook`.
fn generate_updated_filter_rules(
    uninstalled_ip_routines: &netfilter::parser::FilterRoutines,
    installed_ip_routines: &netfilter::parser::FilterRoutines,
    interface_id: NonZeroU64,
    current_installed_rule_index: u32,
) -> Vec<fnet_filter_ext::Rule> {
    let netfilter::parser::FilterRoutines {
        local_ingress: uninstalled_local_ingress,
        local_egress: uninstalled_local_egress,
    } = uninstalled_ip_routines;
    let netfilter::parser::FilterRoutines { local_ingress, local_egress } = installed_ip_routines;

    // Use the same rule index for all rules created for the
    // interface. It is assumed that all `Rule`s across the
    // `FilterRoutines` will inserted in tandem.
    let local_ingress_rule = local_ingress.clone().map(|routine_id| {
        create_interface_matching_jump_rule(
            routine_id,
            current_installed_rule_index,
            interface_id,
            IpHook::LocalIngress,
            &uninstalled_local_ingress
                .as_ref()
                .expect("there should be a corresponding uninstalled routine for local ingress")
                .name,
        )
    });
    let local_egress_rule = local_egress.clone().map(|routine_id| {
        create_interface_matching_jump_rule(
            routine_id,
            current_installed_rule_index,
            interface_id,
            IpHook::LocalEgress,
            &uninstalled_local_egress
                .as_ref()
                .expect("there should be a corresponding uninstalled routine for local egress")
                .name,
        )
    });

    let rules: Vec<_> =
        vec![local_ingress_rule, local_egress_rule].into_iter().filter_map(|rule| rule).collect();

    rules
}

fn create_interface_matching_jump_rule(
    routine_id: RoutineId,
    index: u32,
    interface_id: NonZeroU64,
    hook: IpHook,
    target_routine_name: &str,
) -> fnet_filter_ext::Rule {
    // Some matchers cannot be used on all `IpHook`s.
    let (in_interface, out_interface) = match hook {
        IpHook::LocalIngress | IpHook::Ingress => (Some(InterfaceMatcher::Id(interface_id)), None),
        IpHook::LocalEgress | IpHook::Egress => (None, Some(InterfaceMatcher::Id(interface_id))),
        IpHook::Forwarding => {
            (Some(InterfaceMatcher::Id(interface_id)), Some(InterfaceMatcher::Id(interface_id)))
        }
    };

    // Full path qualification is preferred where types can get mistaken
    // between filtering libraries.
    fnet_filter_ext::Rule {
        id: RuleId { routine: routine_id, index },
        matchers: Matchers { in_interface, out_interface, ..Default::default() },
        action: fnet_filter_ext::Action::Jump(target_routine_name.to_string()),
    }
}

// We use Compare-And-Swap (CAS) protocol to update filter rules. $get_rules returns the current
// generation number. $update_rules will send it with new rules to make sure we are updating the
// intended generation. If the generation number doesn't match, $update_rules will return a
// GenerationMismatch error, then we have to restart from $get_rules.

pub(crate) const FILTER_CAS_RETRY_MAX: i32 = 3;
pub(crate) const FILTER_CAS_RETRY_INTERVAL_MILLIS: i64 = 500;

macro_rules! cas_filter_rules {
    ($filter:expr, $get_rules:ident, $update_rules:ident, $rules:expr, $error_type:ident) => {
        for retry in 0..FILTER_CAS_RETRY_MAX {
            let (_rules, generation) =
                $filter.$get_rules().await.unwrap_or_else(|err| exit_with_fidl_error(err));

            match $filter
                .$update_rules(&$rules, generation)
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
            {
                Ok(()) => {
                    break;
                }
                Err(fnet_filter_deprecated::$error_type::GenerationMismatch)
                    if retry < FILTER_CAS_RETRY_MAX - 1 =>
                {
                    fuchsia_async::Timer::new(
                        FILTER_CAS_RETRY_INTERVAL_MILLIS.millis().after_now(),
                    )
                    .await;
                }
                Err(e) => {
                    bail!("{} failed: {:?}", stringify!($update_rules), e);
                }
            }
        }
    };
}

// This is a placeholder macro while some update operations are not supported.
macro_rules! no_update_filter_rules {
    ($filter:expr, $get_rules:ident, $update_rules:ident, $rules:expr, $error_type:ident) => {
        let (_rules, generation) =
            $filter.$get_rules().await.unwrap_or_else(|err| exit_with_fidl_error(err));

        match $filter
            .$update_rules(&$rules, generation)
            .await
            .unwrap_or_else(|err| exit_with_fidl_error(err))
        {
            Ok(()) => {}
            Err(fnet_filter_deprecated::$error_type::NotSupported) => {
                error!("{} not supported", stringify!($update_rules));
            }
        }
    };
}

async fn update_filters_deprecated(
    filter: &mut fnet_filter_deprecated::FilterProxy,
    config: FilterConfig,
) -> Result<(), anyhow::Error> {
    let FilterConfig { rules, nat_rules, rdr_rules } = config;

    if !rules.is_empty() {
        let rules = netfilter::parser_deprecated::parse_str_to_rules(&rules.join(""))
            .context("error parsing filter rules")?;
        cas_filter_rules!(filter, get_rules, update_rules, rules, FilterUpdateRulesError);
    }

    if !nat_rules.is_empty() {
        let nat_rules = netfilter::parser_deprecated::parse_str_to_nat_rules(&nat_rules.join(""))
            .context("error parsing NAT rules")?;
        cas_filter_rules!(
            filter,
            get_nat_rules,
            update_nat_rules,
            nat_rules,
            FilterUpdateNatRulesError
        );
    }

    if !rdr_rules.is_empty() {
        let rdr_rules = netfilter::parser_deprecated::parse_str_to_rdr_rules(&rdr_rules.join(""))
            .context("error parsing RDR rules")?;
        // TODO(https://fxbug.dev/42147284): Change this to cas_filter_rules once
        // update is supported.
        no_update_filter_rules!(
            filter,
            get_rdr_rules,
            update_rdr_rules,
            rdr_rules,
            FilterUpdateRdrRulesError
        );
    }

    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct FilterEnabledState {
    interface_types: HashSet<InterfaceType>,
    masquerade_enabled_interface_ids: HashSet<NonZeroU64>,
    // Indexed by interface id and stores `RuleId`s inserted for that interface.
    // All rules for an interface should be removed upon interface removal.
    // Vec will always be empty when using filter.deprecated.
    currently_enabled_interfaces: HashMap<NonZeroU64, Vec<RuleId>>,
}

impl FilterEnabledState {
    pub(super) fn new(interface_types: HashSet<InterfaceType>) -> Self {
        Self { interface_types, ..Default::default() }
    }

    /// Updates the filter state for the provided `interface_id` using either
    /// fuchsia.net.filter.deprecated or fuchsia.net.filter.
    ///
    /// `interface_type`: The type of the given interface. If the type cannot be
    /// determined, this will be None, and `FilterEnabledState::interface_types`
    /// will be ignored.
    pub(super) async fn maybe_update(
        &mut self,
        interface_type: Option<InterfaceType>,
        interface_id: NonZeroU64,
        filter: &mut FilterControl,
    ) -> Result<(), anyhow::Error> {
        match filter {
            FilterControl::Deprecated(proxy) => self
                .maybe_update_deprecated(interface_type, interface_id, proxy)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}")),
            FilterControl::Current(filter_state) => {
                self.maybe_update_current(interface_type, interface_id, filter_state).await
            }
        }
    }

    pub(super) async fn maybe_update_deprecated<
        Filter: fnet_filter_deprecated::FilterProxyInterface,
    >(
        &mut self,
        interface_type: Option<InterfaceType>,
        interface_id: NonZeroU64,
        filter: &Filter,
    ) -> Result<(), fnet_filter_deprecated::EnableDisableInterfaceError> {
        let should_be_enabled = self.should_enable(interface_type, interface_id);
        let is_enabled = self.currently_enabled_interfaces.entry(interface_id);

        match (should_be_enabled, is_enabled) {
            (true, Entry::Vacant(entry)) => {
                if let Err(e) = filter
                    .enable_interface(interface_id.get())
                    .await
                    .unwrap_or_else(|err| exit_with_fidl_error(err))
                {
                    error!("failed to enable interface {interface_id}: {e:?}");
                    return Err(e);
                }
                let _ = entry.insert(vec![]);
            }
            (false, Entry::Occupied(entry)) => {
                if let Err(e) = filter
                    .disable_interface(interface_id.get())
                    .await
                    .unwrap_or_else(|err| exit_with_fidl_error(err))
                {
                    error!("failed to disable interface {interface_id}: {e:?}");
                    return Err(e);
                }
                let _ = entry.remove();
            }
            (true, Entry::Occupied(_)) | (false, Entry::Vacant(_)) => {
                // Do nothing. The interface's current state aligns with
                // whether it is present in the map.
            }
        }
        Ok(())
    }

    pub(super) async fn maybe_update_current(
        &mut self,
        interface_type: Option<InterfaceType>,
        interface_id: NonZeroU64,
        filter: &mut FilterState,
    ) -> Result<(), anyhow::Error> {
        let should_be_enabled = self.should_enable(interface_type, interface_id);
        let is_enabled = self.currently_enabled_interfaces.entry(interface_id);

        match (should_be_enabled, is_enabled) {
            (true, Entry::Vacant(entry)) => {
                let FilterState {
                    controller,
                    uninstalled_ip_routines,
                    installed_ip_routines,
                    current_installed_rule_index,
                } = filter;
                let rules = generate_updated_filter_rules(
                    uninstalled_ip_routines,
                    installed_ip_routines,
                    interface_id,
                    *current_installed_rule_index,
                );

                if !rules.is_empty() {
                    let rule_changes = rules
                        .clone()
                        .into_iter()
                        .map(|rule| Change::Create(Resource::Rule(rule)))
                        .collect();
                    controller
                        .push_changes(rule_changes)
                        .await
                        .context("failed to push rules to filter controller")?;
                    controller
                        .commit()
                        .await
                        .context("failed to commit changes to filter controller")?;
                    info!(
                        "new filter rules for iface with id {interface_id:?} \
                                have been committed successfully"
                    );
                    // Increment the current rule index only on success since
                    // `commit` will either apply changes in entirety, or none
                    // at all.
                    *current_installed_rule_index = current_installed_rule_index.wrapping_add(1);
                }

                // Get the `RuleId`s from the inserted `Rule`s so that they can be
                // removed if the interface is disabled.
                let rule_ids: Vec<_> = rules.into_iter().map(|rule| rule.id).collect();
                let _ = entry.insert(rule_ids);
            }
            (false, Entry::Occupied(entry)) => {
                let FilterState { controller, .. } = filter;
                let rule_changes: Vec<_> = entry
                    .remove()
                    .into_iter()
                    .map(|rule_id| Change::Remove(ResourceId::Rule(rule_id)))
                    .collect();

                if !rule_changes.is_empty() {
                    controller
                        .push_changes(rule_changes)
                        .await
                        .context("failed to push remove rule changes to filter controller")?;
                    controller
                        .commit()
                        .await
                        .context("failed to commit changes to filter controller")?;
                    info!(
                        "removal of filter rules for iface with id {interface_id:?} \
                                have been committed successfully"
                    );
                }
            }
            (true, Entry::Occupied(_)) | (false, Entry::Vacant(_)) => {
                // Do nothing. The interface's current state aligns with
                // whether it is present in the map.
            }
        }

        Ok(())
    }

    /// Determines whether a given `interface_id` should be enabled.
    ///
    /// `interface_type`: The type of the given interface. If the type cannot be
    /// determined, this will be None, and `FilterEnabledState::interface_types`
    /// will be ignored.
    fn should_enable(
        &self,
        interface_type: Option<InterfaceType>,
        interface_id: NonZeroU64,
    ) -> bool {
        interface_type
            .as_ref()
            .map(|ty| match ty {
                InterfaceType::Wlan | InterfaceType::Ethernet => self.interface_types.contains(ty),
                // An AP device can be filtered by specifying AP or WLAN.
                InterfaceType::Ap => {
                    self.interface_types.contains(ty)
                        | self.interface_types.contains(&InterfaceType::Wlan)
                }
            })
            .unwrap_or(false)
            || self.masquerade_enabled_interface_ids.contains(&interface_id)
    }

    pub(crate) fn enable_masquerade_interface_id(&mut self, interface_id: NonZeroU64) {
        let _: bool = self.masquerade_enabled_interface_ids.insert(interface_id);
    }

    pub(crate) fn disable_masquerade_interface_id(&mut self, interface_id: NonZeroU64) {
        let _: bool = self.masquerade_enabled_interface_ids.remove(&interface_id);
    }
}

#[cfg(test)]
mod tests {
    use const_unwrap::const_unwrap_option;
    use test_case::test_case;

    use crate::interface::DeviceInfoRef;

    use super::*;

    const INTERFACE_ID: NonZeroU64 = const_unwrap_option(NonZeroU64::new(1));
    const LOCAL_INGRESS: &str = "local_ingress";
    const UNINSTALLED_LOCAL_INGRESS: &str = "local_ingress_uninstalled";
    const LOCAL_EGRESS: &str = "local_egress";
    const UNINSTALLED_LOCAL_EGRESS: &str = "local_egress_uninstalled";

    fn get_foundational_changes() -> Vec<Change> {
        let mut changes = vec![Change::Create(Resource::Namespace(Namespace {
            id: namespace_id(),
            domain: Domain::AllIp,
        }))];

        let local_ingress = (LOCAL_INGRESS, UNINSTALLED_LOCAL_INGRESS, IpHook::LocalIngress);
        let local_egress = (LOCAL_EGRESS, UNINSTALLED_LOCAL_EGRESS, IpHook::LocalEgress);

        let routine_changes = vec![local_ingress, local_egress]
            .into_iter()
            .map(|(installed_name, uninstalled_name, hook)| {
                vec![
                    Routine {
                        id: RoutineId {
                            namespace: namespace_id(),
                            name: String::from(uninstalled_name),
                        },
                        routine_type: RoutineType::Ip(None),
                    },
                    Routine {
                        id: RoutineId {
                            namespace: namespace_id(),
                            name: String::from(installed_name),
                        },
                        routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                            hook,
                            priority: 0i32,
                        })),
                    },
                ]
            })
            .flatten()
            .map(|routine| Change::Create(Resource::Routine(routine)));
        changes.extend(routine_changes);

        changes
    }

    fn create_rule(
        routine: RoutineId,
        index: u32,
        action: fnet_filter_ext::Action,
    ) -> fnet_filter_ext::Rule {
        fnet_filter_ext::Rule {
            id: RuleId { routine, index },
            matchers: Matchers::default(),
            action,
        }
    }

    fn create_routine_id(name: &str) -> RoutineId {
        RoutineId { namespace: namespace_id(), name: String::from(name) }
    }

    fn create_filter_routines(
        namespace: NamespaceId,
        local_ingress: &str,
        local_egress: &str,
    ) -> netfilter::parser::FilterRoutines {
        netfilter::parser::FilterRoutines {
            local_ingress: Some(RoutineId {
                namespace: namespace.clone(),
                name: local_ingress.to_owned(),
            }),
            local_egress: Some(RoutineId { namespace, name: local_egress.to_owned() }),
        }
    }

    // This test only checks for `Ok` cases. The only possible failures for the function under
    // test are related to Rule parsing, which the netfilter library already tests.
    #[test_case(vec![], vec![]; "no_rules")]
    #[test_case(
        vec!["pass in;"],
        vec![create_rule(
                create_routine_id(UNINSTALLED_LOCAL_INGRESS),
                0,
                fnet_filter_ext::Action::Accept,
            )]; "ingress_accept")]
    #[test_case(
        vec!["drop out;"],
        vec![create_rule(
                create_routine_id(UNINSTALLED_LOCAL_EGRESS),
                0,
                fnet_filter_ext::Action::Drop,
            )]; "egress_drop")]
    #[test_case(
        vec!["pass in; drop out;"],
        vec![create_rule(
                create_routine_id(UNINSTALLED_LOCAL_INGRESS),
                0,
                fnet_filter_ext::Action::Accept),
            create_rule(
                create_routine_id(UNINSTALLED_LOCAL_EGRESS),
                1,
                fnet_filter_ext::Action::Drop,
            )]; "ingress_accept_egress_drop")]
    fn test_initial_filter_changes(
        rules_input: Vec<&str>,
        expected_rules: Vec<fnet_filter_ext::Rule>,
    ) {
        let namespace = namespace_id();
        let installed_filter_routines =
            create_filter_routines(namespace.clone(), LOCAL_INGRESS, LOCAL_EGRESS);
        let uninstalled_filter_routines =
            create_filter_routines(namespace, UNINSTALLED_LOCAL_INGRESS, UNINSTALLED_LOCAL_EGRESS);

        let changes = generate_initial_filter_changes(
            &uninstalled_filter_routines,
            &installed_filter_routines,
            FilterConfig {
                rules: rules_input.into_iter().map(|rule| rule.to_owned()).collect(),
                nat_rules: vec![],
                rdr_rules: vec![],
            },
        )
        .expect("rules should be formatted correctly");

        let mut expected_changes = get_foundational_changes();
        let expected_rule_changes =
            expected_rules.into_iter().map(|rule| Change::Create(Resource::Rule(rule)));
        expected_changes.extend(expected_rule_changes);

        assert_eq!(changes, expected_changes);
    }

    #[test]
    fn test_generate_updated_filter_rules() {
        let namespace = namespace_id();
        let installed_filter_routines =
            create_filter_routines(namespace.clone(), LOCAL_INGRESS, LOCAL_EGRESS);
        let uninstalled_filter_routines =
            create_filter_routines(namespace, UNINSTALLED_LOCAL_INGRESS, UNINSTALLED_LOCAL_EGRESS);

        let rules = generate_updated_filter_rules(
            &uninstalled_filter_routines,
            &installed_filter_routines,
            INTERFACE_ID,
            0,
        );

        let local_ingress = (
            installed_filter_routines.local_ingress.unwrap(),
            uninstalled_filter_routines.local_ingress.unwrap().name,
            IpHook::LocalIngress,
        );
        let local_egress = (
            installed_filter_routines.local_egress.unwrap(),
            uninstalled_filter_routines.local_egress.unwrap().name,
            IpHook::LocalEgress,
        );
        let expected_rules: Vec<_> = vec![local_ingress, local_egress]
            .into_iter()
            .map(|(installed_routine, uninstalled_routine_name, hook)| {
                create_interface_matching_jump_rule(
                    installed_routine,
                    0,
                    INTERFACE_ID,
                    hook,
                    &uninstalled_routine_name,
                )
            })
            .collect();

        assert_eq!(rules, expected_rules);
    }

    #[test]
    fn test_should_enable_filter() {
        let types_empty: HashSet<InterfaceType> = [].iter().cloned().collect();
        let types_ethernet: HashSet<InterfaceType> =
            [InterfaceType::Ethernet].iter().cloned().collect();
        let types_wlan: HashSet<InterfaceType> = [InterfaceType::Wlan].iter().cloned().collect();
        let types_ap: HashSet<InterfaceType> = [InterfaceType::Ap].iter().cloned().collect();

        let id = const_unwrap_option(NonZeroU64::new(10));

        let make_info = |device_class| DeviceInfoRef {
            device_class,
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            topological_path: "",
        };

        let wlan_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::Wlan);
        let wlan_ap_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::WlanAp);
        let ethernet_info = make_info(fidl_fuchsia_hardware_network::DeviceClass::Ethernet);

        let mut fes = FilterEnabledState::new(types_empty);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), false);
        assert_eq!(fes.should_enable(Some(wlan_ap_info.interface_type()), id), false);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), false);

        fes.enable_masquerade_interface_id(id);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), true);

        let mut fes = FilterEnabledState::new(types_ethernet);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), false);
        assert_eq!(fes.should_enable(Some(wlan_ap_info.interface_type()), id), false);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), true);

        fes.enable_masquerade_interface_id(id);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), true);

        let mut fes = FilterEnabledState::new(types_wlan);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), true);
        assert_eq!(fes.should_enable(Some(wlan_ap_info.interface_type()), id), true);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), false);

        fes.enable_masquerade_interface_id(id);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), true);

        let mut fes = FilterEnabledState::new(types_ap);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), false);
        assert_eq!(fes.should_enable(Some(wlan_ap_info.interface_type()), id), true);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), false);

        fes.enable_masquerade_interface_id(id);
        assert_eq!(fes.should_enable(Some(wlan_info.interface_type()), id), true);
        assert_eq!(fes.should_enable(Some(ethernet_info.interface_type()), id), true);
    }
}
