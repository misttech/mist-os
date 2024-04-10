// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{collections::HashSet, num::NonZeroU64};

use fidl_fuchsia_net_filter_deprecated as fnet_filter;
use fuchsia_async::DurationExt as _;
use fuchsia_zircon::DurationNum as _;

use anyhow::{bail, Context as _};
use tracing::error;

use crate::{exit_with_fidl_error, FilterConfig, InterfaceType};

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
                Err(fnet_filter::$error_type::GenerationMismatch)
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
            Err(fnet_filter::$error_type::NotSupported) => {
                error!("{} not supported", stringify!($update_rules));
            }
        }
    };
}

/// Updates the network filter configurations.
pub(crate) async fn update_filters(
    filter: &mut fnet_filter::FilterProxy,
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
        // TODO(https://fxbug.dev/42147284): Change this to cas_filter_rules once update is supported.
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
pub(crate) struct FilterEnabledState {
    interface_types: HashSet<InterfaceType>,
    masquerade_enabled_interface_ids: HashSet<NonZeroU64>,
    currently_enabled_interfaces: HashSet<NonZeroU64>,
}

impl FilterEnabledState {
    pub(crate) fn new(interface_types: HashSet<InterfaceType>) -> Self {
        Self { interface_types, ..Default::default() }
    }

    /// Updates the filter state for the provided `interface_id`
    ///
    /// `interface_type`: The type of the given interface. If the type cannot be
    /// determined, this will be None, and `FilterEnabledState::interface_types`
    /// will be ignored.
    pub(crate) async fn maybe_update<Filter: fnet_filter::FilterProxyInterface>(
        &mut self,
        interface_type: Option<InterfaceType>,
        interface_id: NonZeroU64,
        filter: &Filter,
    ) -> Result<(), fnet_filter::EnableDisableInterfaceError> {
        let should_be_enabled = self.should_enable(interface_type, interface_id);
        let is_enabled = self.currently_enabled_interfaces.contains(&interface_id);
        if should_be_enabled && !is_enabled {
            if let Err(e) = filter
                .enable_interface(interface_id.get())
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
            {
                error!("failed to enable interface {interface_id}: {e:?}");
                return Err(e);
            }
            let _: bool = self.currently_enabled_interfaces.insert(interface_id);
        } else if !should_be_enabled && is_enabled {
            if let Err(e) = filter
                .disable_interface(interface_id.get())
                .await
                .unwrap_or_else(|err| exit_with_fidl_error(err))
            {
                error!("failed to disable interface {interface_id}: {e:?}");
                return Err(e);
            }
            let _: bool = self.currently_enabled_interfaces.remove(&interface_id);
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

    use crate::interface::DeviceInfoRef;

    use super::*;

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
