// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device link layer configuration types.

use lock_order::Locked;
use net_types::ip::{Ipv4, Ipv6};

use crate::{
    device::{id::Id as _, DeviceId},
    ip::device::nud::{NudUserConfig, NudUserConfigUpdate},
    NonSyncContext, SyncCtx,
};

/// Device ARP configuration.
#[derive(Clone, Debug)]
pub struct ArpConfiguration {
    /// NUD over ARP configuration.
    pub nud: NudUserConfig,
}

/// Device NDP configuration.
#[derive(Clone, Debug)]
pub struct NdpConfiguration {
    /// NUD over NDP configuration.
    pub nud: NudUserConfig,
}

/// Device link layer configuration.
#[derive(Clone, Debug)]
pub struct DeviceConfiguration {
    /// ARP configurations.
    ///
    /// Only present if the device supports NDP.
    pub arp: Option<ArpConfiguration>,
    /// NDP configurations.
    ///
    /// Only present if the device supports NDP.
    pub ndp: Option<NdpConfiguration>,
}
/// An update to apply to ARP configurations.
///
/// Only fields with variant `Some` are requested to be updated.
#[derive(Clone, Debug, Default)]
pub struct ArpConfigurationUpdate {
    /// NUD over ARP configuration update.
    pub nud: Option<NudUserConfigUpdate>,
}

/// An update to apply to NDP configurations.
///
/// Only fields with variant `Some` are requested to be updated.
#[derive(Clone, Debug, Default)]
pub struct NdpConfigurationUpdate {
    /// NUD over NDP configuration update.
    pub nud: Option<NudUserConfigUpdate>,
}

/// An update to apply to device configurations.
///
/// Only fields with variant `Some` are requested to be updated.
#[derive(Clone, Debug, Default)]
pub struct DeviceConfigurationUpdate {
    /// ARP configuration update.
    pub arp: Option<ArpConfigurationUpdate>,
    /// NDP configuration update.
    pub ndp: Option<NdpConfigurationUpdate>,
}

/// Errors observed updating device configuration.
#[derive(Debug)]
pub enum DeviceConfigurationUpdateError {
    /// ARP is not supported for the requested device.
    ArpNotSupported,
    /// NDP is not supported for the requested device.
    NdpNotSupported,
}

/// Pending device configuration update.
///
/// Configuration is only applied when the `apply` method is called.
pub struct PendingDeviceConfigurationUpdate<'a, D>(DeviceConfigurationUpdate, &'a D);

impl<'a, C> PendingDeviceConfigurationUpdate<'a, DeviceId<C>>
where
    C: NonSyncContext,
{
    /// Applies the configuration and returns a [`DeviceConfigurationUpdate`]
    /// with the previous values for all configurations for all `Some` fields.
    ///
    /// Note that even if the previous value matched the requested value, it is
    /// still populated in the returned `DeviceConfigurationUpdate`.
    pub fn apply(self, core_ctx: &SyncCtx<C>) -> DeviceConfigurationUpdate {
        let Self(DeviceConfigurationUpdate { arp, ndp }, device_id) = self;
        let eth = match device_id {
            DeviceId::Loopback(_) => {
                // Loopback currently doesn't support any configuration.
                // Validation happens when we create a new pending configuration
                // update we can assert the invariant here.
                debug_assert!(arp.is_none());
                debug_assert!(ndp.is_none());
                return DeviceConfigurationUpdate::default();
            }
            DeviceId::Ethernet(eth) => eth,
        };
        crate::device::integration::with_ethernet_state(
            &mut Locked::new(core_ctx),
            eth,
            |mut state| {
                let arp = arp.map(|ArpConfigurationUpdate { nud }| {
                    let nud = nud.map(|config| {
                        config.apply_and_take_previous(
                            &mut *state.write_lock::<crate::lock_ordering::NudConfig<Ipv4>>(),
                        )
                    });
                    ArpConfigurationUpdate { nud }
                });
                let ndp = ndp.map(|NdpConfigurationUpdate { nud }| {
                    let nud = nud.map(|config| {
                        config.apply_and_take_previous(
                            &mut *state.write_lock::<crate::lock_ordering::NudConfig<Ipv6>>(),
                        )
                    });
                    NdpConfigurationUpdate { nud }
                });
                DeviceConfigurationUpdate { arp, ndp }
            },
        )
    }
}

/// Creates a new device configuration update for the given device.
pub fn new_device_configuration_update<C: NonSyncContext>(
    device: &DeviceId<C>,
    config: DeviceConfigurationUpdate,
) -> Result<PendingDeviceConfigurationUpdate<'_, DeviceId<C>>, DeviceConfigurationUpdateError> {
    let DeviceConfigurationUpdate { arp, ndp } = &config;
    if device.is_loopback() {
        if arp.is_some() {
            return Err(DeviceConfigurationUpdateError::ArpNotSupported);
        }
        if ndp.is_some() {
            return Err(DeviceConfigurationUpdateError::NdpNotSupported);
        }
    }
    Ok(PendingDeviceConfigurationUpdate(config, device))
}

/// Returns a snapshot of the given device's configuration.
pub fn get_device_configuration<C: NonSyncContext>(
    core_ctx: &SyncCtx<C>,
    device_id: &DeviceId<C>,
) -> DeviceConfiguration {
    match device_id {
        DeviceId::Loopback(_) => DeviceConfiguration { arp: None, ndp: None },
        DeviceId::Ethernet(eth) => crate::device::integration::with_ethernet_state(
            &mut Locked::new(core_ctx),
            eth,
            |mut state| {
                let arp = Some(ArpConfiguration {
                    nud: state.read_lock::<crate::lock_ordering::NudConfig<Ipv4>>().clone(),
                });
                let ndp = Some(NdpConfiguration {
                    nud: state.read_lock::<crate::lock_ordering::NudConfig<Ipv6>>().clone(),
                });
                DeviceConfiguration { arp, ndp }
            },
        ),
    }
}
