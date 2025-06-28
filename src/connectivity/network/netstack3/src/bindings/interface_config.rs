// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU16;

use net_types::ip::IpVersion;
use netstack3_core::device::{
    ArpConfiguration, ArpConfigurationUpdate, DeviceConfiguration, DeviceConfigurationUpdate,
    NdpConfiguration, NdpConfigurationUpdate,
};
use netstack3_core::ip::{
    IgmpConfigMode, IidGenerationConfiguration, IpDeviceConfiguration, IpDeviceConfigurationUpdate,
    Ipv4DeviceConfiguration, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfiguration,
    Ipv6DeviceConfigurationUpdate, MldConfigMode, SlaacConfiguration, SlaacConfigurationUpdate,
    StableSlaacAddressConfiguration, TemporarySlaacAddressConfiguration,
};
use netstack3_core::neighbor::{NudUserConfig, NudUserConfigUpdate};
use {
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_settings as fnet_settings,
};

use crate::bindings::util::{IllegalNonPositiveValueError, IntoFidl, TryIntoCore as _};

/// Unified view of netstack interface configuration.
#[derive(Clone)]
pub(crate) struct InterfaceConfig<D> {
    pub(crate) device: D,
    pub(crate) ipv4: Ipv4DeviceConfiguration,
    pub(crate) ipv6: Ipv6DeviceConfiguration,
    pub(crate) igmp: IgmpConfigMode,
    pub(crate) mld: MldConfigMode,
}

impl<D> InterfaceConfig<D> {
    pub(crate) fn new(defaults: &InterfaceConfigDefaults) -> Self
    where
        D: Default,
    {
        let InterfaceConfigDefaults { opaque_iids } = defaults;
        let ip_config = |ip_version: IpVersion| {
            let dad_transmits = match ip_version {
                IpVersion::V4 => {
                    Ipv4DeviceConfiguration::DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS
                }
                IpVersion::V6 => {
                    Ipv6DeviceConfiguration::DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS
                }
            };
            IpDeviceConfiguration {
                unicast_forwarding_enabled: false,
                multicast_forwarding_enabled: false,
                gmp_enabled: true,
                dad_transmits: Some(dad_transmits),
            }
        };

        let ipv4 = Ipv4DeviceConfiguration { ip_config: ip_config(IpVersion::V4) };

        let iid_generation = if *opaque_iids {
            IidGenerationConfiguration::Opaque {
                idgen_retries: StableSlaacAddressConfiguration::DEFAULT_IDGEN_RETRIES,
            }
        } else {
            IidGenerationConfiguration::Eui64
        };

        let ipv6 = Ipv6DeviceConfiguration {
            max_router_solicitations: Some(Ipv6DeviceConfiguration::DEFAULT_MAX_RTR_SOLICITATIONS),
            slaac_config: SlaacConfiguration {
                stable_address_configuration: StableSlaacAddressConfiguration::Enabled {
                    iid_generation,
                },
                temporary_address_configuration:
                    TemporarySlaacAddressConfiguration::enabled_with_rfc_defaults(),
            },
            ip_config: ip_config(IpVersion::V6),
        };

        Self { device: D::default(), ipv4, ipv6, igmp: IgmpConfigMode::V3, mld: MldConfigMode::V2 }
    }

    fn to_core_ip_update(
        ip_config: &IpDeviceConfiguration,
        config_type: InterfaceConfigType,
    ) -> IpDeviceConfigurationUpdate {
        let IpDeviceConfiguration {
            gmp_enabled,
            unicast_forwarding_enabled,
            multicast_forwarding_enabled,
            dad_transmits,
        } = ip_config;

        let dad_transmits = match config_type {
            InterfaceConfigType::Ethernet | InterfaceConfigType::PureIp => *dad_transmits,
            InterfaceConfigType::Blackhole | InterfaceConfigType::Loopback => None,
        };

        let gmp_enabled = match config_type {
            InterfaceConfigType::Ethernet | InterfaceConfigType::PureIp => *gmp_enabled,
            InterfaceConfigType::Blackhole | InterfaceConfigType::Loopback => false,
        };

        IpDeviceConfigurationUpdate {
            ip_enabled: Some(config_type.default_enabled()),
            unicast_forwarding_enabled: Some(*unicast_forwarding_enabled),
            multicast_forwarding_enabled: Some(*multicast_forwarding_enabled),
            gmp_enabled: Some(gmp_enabled),
            dad_transmits: Some(dad_transmits),
        }
    }

    pub(crate) fn to_core_ipv4_update(
        &self,
        config_type: InterfaceConfigType,
    ) -> Ipv4DeviceConfigurationUpdate {
        let Ipv4DeviceConfiguration { ip_config } = &self.ipv4;
        Ipv4DeviceConfigurationUpdate {
            ip_config: Self::to_core_ip_update(ip_config, config_type),
            igmp_mode: Some(self.igmp),
        }
    }

    pub(crate) fn to_core_ipv6_update(
        &self,
        config_type: InterfaceConfigType,
    ) -> Ipv6DeviceConfigurationUpdate {
        let Ipv6DeviceConfiguration {
            max_router_solicitations,
            slaac_config:
                SlaacConfiguration { stable_address_configuration, temporary_address_configuration },
            ip_config,
        } = &self.ipv6;

        let ip_config = Self::to_core_ip_update(ip_config, config_type);

        let max_router_solicitations = match config_type {
            InterfaceConfigType::Ethernet | InterfaceConfigType::PureIp => {
                *max_router_solicitations
            }
            InterfaceConfigType::Blackhole | InterfaceConfigType::Loopback => None,
        };

        let (stable_address_configuration, temporary_address_configuration) = match config_type {
            InterfaceConfigType::Ethernet | InterfaceConfigType::PureIp => {
                (*stable_address_configuration, *temporary_address_configuration)
            }
            InterfaceConfigType::Blackhole | InterfaceConfigType::Loopback => (
                StableSlaacAddressConfiguration::Disabled,
                TemporarySlaacAddressConfiguration::Disabled,
            ),
        };

        let slaac_config = SlaacConfigurationUpdate {
            stable_address_configuration: Some(stable_address_configuration),
            temporary_address_configuration: Some(temporary_address_configuration),
        };

        Ipv6DeviceConfigurationUpdate {
            ip_config,
            max_router_solicitations: Some(max_router_solicitations),
            slaac_config,
            mld_mode: Some(self.mld),
        }
    }
}

impl InterfaceConfig<DeviceNeighborConfig> {
    pub(crate) fn to_core_device_update(
        &self,
        config_type: InterfaceConfigType,
    ) -> DeviceConfigurationUpdate {
        let DeviceNeighborConfig { arp, ndp } = &self.device;

        let to_nud_update = |nud: &NudUserConfig| {
            let NudUserConfig {
                max_unicast_solicitations,
                max_multicast_solicitations,
                base_reachable_time,
            } = nud;
            NudUserConfigUpdate {
                max_unicast_solicitations: Some(*max_unicast_solicitations),
                max_multicast_solicitations: Some(*max_multicast_solicitations),
                base_reachable_time: Some(*base_reachable_time),
            }
        };

        let supports_arp_ndp = match config_type {
            InterfaceConfigType::Ethernet => true,
            InterfaceConfigType::PureIp
            | InterfaceConfigType::Blackhole
            | InterfaceConfigType::Loopback => false,
        };

        let ArpConfiguration { nud } = arp;
        let arp =
            supports_arp_ndp.then(|| ArpConfigurationUpdate { nud: Some(to_nud_update(nud)) });

        let NdpConfiguration { nud } = ndp;
        let ndp =
            supports_arp_ndp.then(|| NdpConfigurationUpdate { nud: Some(to_nud_update(nud)) });

        DeviceConfigurationUpdate { arp, ndp }
    }

    pub(crate) fn update(&mut self, update: InterfaceConfigUpdate) -> InterfaceConfigUpdate {
        let InterfaceConfigUpdate { ipv4, ipv6, device } = update;
        let ipv4 = ipv4.map(|u| self.update_ipv4(u));
        let ipv6 = ipv6.map(|u| self.update_ipv6(u));
        let device = device.map(|u| self.update_device(u));
        InterfaceConfigUpdate { ipv4, ipv6, device }
    }

    fn update_ip_config(
        config: &mut IpDeviceConfiguration,
        update: IpDeviceConfigurationUpdate,
    ) -> IpDeviceConfigurationUpdate {
        let IpDeviceConfigurationUpdate {
            ip_enabled,
            unicast_forwarding_enabled,
            multicast_forwarding_enabled,
            gmp_enabled,
            dad_transmits,
        } = update;
        // NB: This is likely to change when we export configuration enabling
        // IPv4 and IPv6 separately.
        assert_eq!(ip_enabled, None, "updating global IP enabled not supported");
        IpDeviceConfigurationUpdate {
            ip_enabled: None,
            unicast_forwarding_enabled: update_and_take_prev(
                &mut config.unicast_forwarding_enabled,
                unicast_forwarding_enabled,
            ),
            multicast_forwarding_enabled: update_and_take_prev(
                &mut config.multicast_forwarding_enabled,
                multicast_forwarding_enabled,
            ),
            gmp_enabled: update_and_take_prev(&mut config.gmp_enabled, gmp_enabled),
            dad_transmits: update_and_take_prev(&mut config.dad_transmits, dad_transmits),
        }
    }

    fn update_ipv4(
        &mut self,
        update: Ipv4DeviceConfigurationUpdate,
    ) -> Ipv4DeviceConfigurationUpdate {
        let Ipv4DeviceConfigurationUpdate { ip_config, igmp_mode } = update;
        let ip_config_update = Self::update_ip_config(&mut self.ipv4.ip_config, ip_config);
        let igmp_mode_update = update_and_take_prev(&mut self.igmp, igmp_mode);

        Ipv4DeviceConfigurationUpdate { ip_config: ip_config_update, igmp_mode: igmp_mode_update }
    }

    fn update_ipv6(
        &mut self,
        update: Ipv6DeviceConfigurationUpdate,
    ) -> Ipv6DeviceConfigurationUpdate {
        let Ipv6DeviceConfigurationUpdate {
            max_router_solicitations,
            slaac_config:
                SlaacConfigurationUpdate {
                    stable_address_configuration,
                    temporary_address_configuration,
                },
            ip_config,
            mld_mode,
        } = update;
        let ip_config_update = Self::update_ip_config(&mut self.ipv6.ip_config, ip_config);
        let mld_mode_update = update_and_take_prev(&mut self.mld, mld_mode);
        let max_rtr_solicitations_update =
            update_and_take_prev(&mut self.ipv6.max_router_solicitations, max_router_solicitations);

        let slaac_config_update = SlaacConfigurationUpdate {
            stable_address_configuration: update_and_take_prev(
                &mut self.ipv6.slaac_config.stable_address_configuration,
                stable_address_configuration,
            ),
            temporary_address_configuration: update_and_take_prev(
                &mut self.ipv6.slaac_config.temporary_address_configuration,
                temporary_address_configuration,
            ),
        };

        Ipv6DeviceConfigurationUpdate {
            max_router_solicitations: max_rtr_solicitations_update,
            slaac_config: slaac_config_update,
            ip_config: ip_config_update,
            mld_mode: mld_mode_update,
        }
    }

    fn update_nud_config(
        config: &mut NudUserConfig,
        update: NudUserConfigUpdate,
    ) -> NudUserConfigUpdate {
        let NudUserConfigUpdate {
            max_unicast_solicitations,
            max_multicast_solicitations,
            base_reachable_time,
        } = update;
        NudUserConfigUpdate {
            max_unicast_solicitations: update_and_take_prev(
                &mut config.max_unicast_solicitations,
                max_unicast_solicitations,
            ),
            max_multicast_solicitations: update_and_take_prev(
                &mut config.max_multicast_solicitations,
                max_multicast_solicitations,
            ),
            base_reachable_time: update_and_take_prev(
                &mut config.base_reachable_time,
                base_reachable_time,
            ),
        }
    }

    fn update_device(&mut self, update: DeviceConfigurationUpdate) -> DeviceConfigurationUpdate {
        let DeviceConfigurationUpdate { arp, ndp } = update;

        let arp_update = arp.map(|u| {
            let ArpConfigurationUpdate { nud } = u;
            let nud_update = nud.map(|u| Self::update_nud_config(&mut self.device.arp.nud, u));
            ArpConfigurationUpdate { nud: nud_update }
        });
        let ndp_update = ndp.map(|u| {
            let NdpConfigurationUpdate { nud } = u;
            let nud_update = nud.map(|u| Self::update_nud_config(&mut self.device.ndp.nud, u));
            NdpConfigurationUpdate { nud: nud_update }
        });
        DeviceConfigurationUpdate { arp: arp_update, ndp: ndp_update }
    }
}

#[derive(Clone)]
pub(crate) struct DeviceNeighborConfig {
    pub(crate) arp: ArpConfiguration,
    pub(crate) ndp: NdpConfiguration,
}

impl Default for DeviceNeighborConfig {
    fn default() -> Self {
        Self {
            arp: ArpConfiguration { nud: NudUserConfig::default() },
            ndp: NdpConfiguration { nud: NudUserConfig::default() },
        }
    }
}

impl From<DeviceNeighborConfig> for DeviceConfiguration {
    fn from(value: DeviceNeighborConfig) -> Self {
        let DeviceNeighborConfig { arp, ndp } = value;
        DeviceConfiguration { arp: Some(arp), ndp: Some(ndp) }
    }
}

#[derive(Copy, Clone)]
pub(crate) enum InterfaceConfigType {
    Ethernet,
    PureIp,
    Blackhole,
    Loopback,
}

impl InterfaceConfigType {
    fn default_enabled(&self) -> bool {
        match self {
            Self::Ethernet | Self::Blackhole | Self::PureIp => false,
            // Loopback interface is always created with enabled mode.
            Self::Loopback => true,
        }
    }
}

pub(crate) struct InterfaceConfigUpdate {
    pub(crate) ipv4: Option<Ipv4DeviceConfigurationUpdate>,
    pub(crate) ipv6: Option<Ipv6DeviceConfigurationUpdate>,
    pub(crate) device: Option<DeviceConfigurationUpdate>,
}

/// A helper for converting FIDL and core representations of interface
/// configuration.
pub(crate) struct FidlInterfaceConfig(fnet_interfaces_admin::Configuration);

impl FidlInterfaceConfig {
    /// Creates a complete `FidlInterfaceConfig` from all the information kept by
    /// the stack.
    pub(crate) fn new_complete<D: Into<DeviceConfiguration>>(config: InterfaceConfig<D>) -> Self {
        let InterfaceConfig { device, ipv4, ipv6, igmp, mld } = config;
        let DeviceConfiguration { arp, ndp } = device.into();

        let Ipv4DeviceConfiguration {
            ip_config:
                IpDeviceConfiguration {
                    unicast_forwarding_enabled,
                    multicast_forwarding_enabled,
                    gmp_enabled: _,
                    dad_transmits,
                },
        } = ipv4;
        let nud = arp.map(|ArpConfiguration { nud }| nud);
        let arp = Some(fnet_interfaces_admin::ArpConfiguration {
            nud: nud.map(IntoFidl::into_fidl),
            dad: Some(fnet_interfaces_admin::DadConfiguration {
                transmits: Some(dad_transmits.map_or(0, NonZeroU16::get)),
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            __source_breaking: fidl::marker::SourceBreaking,
        });
        let igmp = fnet_interfaces_admin::IgmpConfiguration {
            version: Some(igmp.into_fidl()),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        let ipv4 = Some(fnet_interfaces_admin::Ipv4Configuration {
            unicast_forwarding: Some(unicast_forwarding_enabled),
            igmp: Some(igmp),
            multicast_forwarding: Some(multicast_forwarding_enabled),
            arp,
            __source_breaking: fidl::marker::SourceBreaking,
        });

        let Ipv6DeviceConfiguration {
            max_router_solicitations: _,
            slaac_config,
            ip_config:
                IpDeviceConfiguration {
                    gmp_enabled: _,
                    unicast_forwarding_enabled,
                    multicast_forwarding_enabled,
                    dad_transmits,
                },
        } = ipv6;
        let nud = ndp.map(|NdpConfiguration { nud }| nud);

        let ndp = Some(fnet_interfaces_admin::NdpConfiguration {
            nud: nud.map(IntoFidl::into_fidl),
            dad: Some(fnet_interfaces_admin::DadConfiguration {
                transmits: Some(dad_transmits.map_or(0, NonZeroU16::get)),
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            slaac: Some(slaac_config.into_fidl()),
            __source_breaking: fidl::marker::SourceBreaking,
        });

        let mld = fnet_interfaces_admin::MldConfiguration {
            version: Some(mld.into_fidl()),
            __source_breaking: fidl::marker::SourceBreaking,
        };

        let ipv6 = Some(fnet_interfaces_admin::Ipv6Configuration {
            unicast_forwarding: Some(unicast_forwarding_enabled),
            mld: Some(mld),
            multicast_forwarding: Some(multicast_forwarding_enabled),
            ndp,
            __source_breaking: fidl::marker::SourceBreaking,
        });

        Self(fnet_interfaces_admin::Configuration {
            ipv4,
            ipv6,
            __source_breaking: fidl::marker::SourceBreaking,
        })
    }

    pub(crate) fn new_update(update: InterfaceConfigUpdate) -> Self {
        let InterfaceConfigUpdate { ipv4, ipv6, device } = update;
        let DeviceConfigurationUpdate { arp, ndp } = device.unwrap_or_default();

        let ipv4 = ipv4.map(|ipv4| {
            let Ipv4DeviceConfigurationUpdate { ip_config, igmp_mode } = ipv4;
            let IpDeviceConfigurationUpdate {
                unicast_forwarding_enabled,
                multicast_forwarding_enabled,
                ip_enabled: _,
                gmp_enabled: _,
                dad_transmits,
            } = ip_config;

            let nud = arp.and_then(|ArpConfigurationUpdate { nud }| nud);

            let dad = dad_transmits.map(|transmits| fnet_interfaces_admin::DadConfiguration {
                transmits: Some(transmits.map_or(0, NonZeroU16::get)),
                __source_breaking: fidl::marker::SourceBreaking,
            });

            let arp = some_if_not_default(fnet_interfaces_admin::ArpConfiguration {
                nud: nud.map(IntoFidl::into_fidl),
                dad,
                __source_breaking: fidl::marker::SourceBreaking,
            });

            let igmp = some_if_not_default(fnet_interfaces_admin::IgmpConfiguration {
                version: igmp_mode.map(IntoFidl::into_fidl),
                __source_breaking: fidl::marker::SourceBreaking,
            });

            fnet_interfaces_admin::Ipv4Configuration {
                unicast_forwarding: unicast_forwarding_enabled,
                multicast_forwarding: multicast_forwarding_enabled,
                igmp,
                arp,
                __source_breaking: fidl::marker::SourceBreaking,
            }
        });

        let ipv6 = ipv6.map(|ipv6| {
            let Ipv6DeviceConfigurationUpdate {
                ip_config,
                slaac_config,
                max_router_solicitations: _,
                mld_mode,
            } = ipv6;
            let IpDeviceConfigurationUpdate {
                unicast_forwarding_enabled,
                multicast_forwarding_enabled,
                ip_enabled: _,
                gmp_enabled: _,
                dad_transmits,
            } = ip_config;

            let nud = ndp.and_then(|NdpConfigurationUpdate { nud }| nud);

            let dad = dad_transmits.map(|transmits| fnet_interfaces_admin::DadConfiguration {
                transmits: Some(transmits.map_or(0, NonZeroU16::get)),
                __source_breaking: fidl::marker::SourceBreaking,
            });

            let slaac = some_if_not_default(slaac_config.into_fidl());
            let ndp = some_if_not_default(fnet_interfaces_admin::NdpConfiguration {
                nud: nud.map(IntoFidl::into_fidl),
                dad,
                slaac,
                __source_breaking: fidl::marker::SourceBreaking,
            });

            let mld = some_if_not_default(fnet_interfaces_admin::MldConfiguration {
                version: mld_mode.map(IntoFidl::into_fidl),
                __source_breaking: fidl::marker::SourceBreaking,
            });
            fnet_interfaces_admin::Ipv6Configuration {
                unicast_forwarding: unicast_forwarding_enabled,
                multicast_forwarding: multicast_forwarding_enabled,
                mld,
                ndp,
                __source_breaking: fidl::marker::SourceBreaking,
            }
        });

        Self(fnet_interfaces_admin::Configuration {
            ipv4,
            ipv6,
            __source_breaking: fidl::marker::SourceBreaking,
        })
    }

    pub(crate) fn try_into_update(self) -> Result<InterfaceConfigUpdate, ConfigError> {
        let Self(fnet_interfaces_admin::Configuration { ipv4, ipv6, __source_breaking }) = self;
        let (ipv4, arp) = match ipv4 {
            Some(fnet_interfaces_admin::Ipv4Configuration {
                igmp,
                multicast_forwarding,
                unicast_forwarding,
                arp,
                __source_breaking,
            }) => {
                let fnet_interfaces_admin::IgmpConfiguration {
                    version: igmp_version,
                    __source_breaking,
                } = igmp.unwrap_or_default();
                let igmp_mode = igmp_version
                    .map(|v| match v {
                        fnet_interfaces_admin::IgmpVersion::V1 => Ok(IgmpConfigMode::V1),
                        fnet_interfaces_admin::IgmpVersion::V2 => Ok(IgmpConfigMode::V2),
                        fnet_interfaces_admin::IgmpVersion::V3 => Ok(IgmpConfigMode::V3),
                        fnet_interfaces_admin::IgmpVersion::__SourceBreaking { .. } => {
                            Err(ConfigError::IgmpVersionNotSupported)
                        }
                    })
                    .transpose()?;

                let fnet_interfaces_admin::ArpConfiguration { nud, dad, __source_breaking } =
                    arp.unwrap_or_default();

                let fnet_interfaces_admin::DadConfiguration {
                    transmits: dad_transmits,
                    __source_breaking,
                } = dad.unwrap_or_default();

                (
                    Some(Ipv4DeviceConfigurationUpdate {
                        ip_config: IpDeviceConfigurationUpdate {
                            unicast_forwarding_enabled: unicast_forwarding,
                            multicast_forwarding_enabled: multicast_forwarding,
                            dad_transmits: dad_transmits.map(|v| NonZeroU16::new(v)),
                            ..Default::default()
                        },
                        igmp_mode,
                    }),
                    nud.map(|nud| {
                        Ok::<_, ConfigError>(ArpConfigurationUpdate {
                            nud: Some(nud.try_into_core()?),
                        })
                    })
                    .transpose()?,
                )
            }
            None => (None, None),
        };

        let (ipv6, ndp) = match ipv6 {
            Some(fnet_interfaces_admin::Ipv6Configuration {
                mld,
                multicast_forwarding,
                unicast_forwarding,
                ndp,
                __source_breaking,
            }) => {
                let fnet_interfaces_admin::MldConfiguration {
                    version: mld_version,
                    __source_breaking,
                } = mld.unwrap_or_default();
                let mld_mode = mld_version
                    .map(|v| match v {
                        fnet_interfaces_admin::MldVersion::V1 => Ok(MldConfigMode::V1),
                        fnet_interfaces_admin::MldVersion::V2 => Ok(MldConfigMode::V2),
                        fnet_interfaces_admin::MldVersion::__SourceBreaking { .. } => {
                            Err(ConfigError::MldVersionNotSupported)
                        }
                    })
                    .transpose()?;

                let fnet_interfaces_admin::NdpConfiguration { nud, dad, slaac, __source_breaking } =
                    ndp.unwrap_or_default();

                let fnet_interfaces_admin::DadConfiguration {
                    transmits: dad_transmits,
                    __source_breaking,
                } = dad.unwrap_or_default();

                let fnet_interfaces_admin::SlaacConfiguration {
                    temporary_address,
                    __source_breaking,
                } = slaac.unwrap_or_default();

                let temporary_address_configuration = temporary_address.map(|enabled| {
                    if enabled {
                        TemporarySlaacAddressConfiguration::enabled_with_rfc_defaults()
                    } else {
                        TemporarySlaacAddressConfiguration::Disabled
                    }
                });

                (
                    Some(Ipv6DeviceConfigurationUpdate {
                        ip_config: IpDeviceConfigurationUpdate {
                            unicast_forwarding_enabled: unicast_forwarding,
                            ip_enabled: None,
                            multicast_forwarding_enabled: multicast_forwarding,
                            gmp_enabled: None,
                            dad_transmits: dad_transmits.map(|v| NonZeroU16::new(v)),
                        },
                        slaac_config: SlaacConfigurationUpdate {
                            temporary_address_configuration,
                            stable_address_configuration: None,
                        },
                        max_router_solicitations: None,
                        mld_mode,
                    }),
                    nud.map(|nud| {
                        Ok::<_, ConfigError>(NdpConfigurationUpdate {
                            nud: Some(nud.try_into_core()?),
                        })
                    })
                    .transpose()?,
                )
            }
            None => (None, None),
        };

        let device = some_if_not_default(DeviceConfigurationUpdate { arp, ndp });

        Ok(InterfaceConfigUpdate { ipv4, ipv6, device })
    }
}

impl From<FidlInterfaceConfig> for fnet_interfaces_admin::Configuration {
    fn from(FidlInterfaceConfig(value): FidlInterfaceConfig) -> Self {
        value
    }
}

impl From<fnet_interfaces_admin::Configuration> for FidlInterfaceConfig {
    fn from(value: fnet_interfaces_admin::Configuration) -> Self {
        Self(value)
    }
}

pub(crate) enum ConfigError {
    IllegalNonPositiveValue(IllegalNonPositiveValueError),
    IgmpVersionNotSupported,
    MldVersionNotSupported,
}

impl From<IllegalNonPositiveValueError> for ConfigError {
    fn from(value: IllegalNonPositiveValueError) -> Self {
        Self::IllegalNonPositiveValue(value)
    }
}

impl From<ConfigError> for fnet_interfaces_admin::ControlSetConfigurationError {
    fn from(value: ConfigError) -> Self {
        match value {
            ConfigError::IllegalNonPositiveValue(v) => v.into_fidl(),
            ConfigError::IgmpVersionNotSupported => {
                fnet_interfaces_admin::ControlSetConfigurationError::Ipv4IgmpVersionUnsupported
            }
            ConfigError::MldVersionNotSupported => {
                fnet_interfaces_admin::ControlSetConfigurationError::Ipv6MldVersionUnsupported
            }
        }
    }
}

impl From<ConfigError> for fnet_settings::UpdateError {
    fn from(value: ConfigError) -> Self {
        match value {
            ConfigError::IllegalNonPositiveValue(e) => match e {
                IllegalNonPositiveValueError::Zero => Self::IllegalZeroValue,
                IllegalNonPositiveValueError::Negative => Self::IllegalNegativeValue,
            },
            ConfigError::IgmpVersionNotSupported | ConfigError::MldVersionNotSupported => {
                Self::NotSupported
            }
        }
    }
}

#[derive(Default)]
pub struct InterfaceConfigDefaults {
    /// Whether to configure interfaces with opaque IIDs by default. Note that the
    /// IID generation method can still be overridden per-interface.
    pub opaque_iids: bool,
}

#[inline]
fn some_if_not_default<T: Default + PartialEq>(t: T) -> Option<T> {
    (t != T::default()).then_some(t)
}

fn update_and_take_prev<T>(slot: &mut T, update: Option<T>) -> Option<T> {
    update.map(|u| std::mem::replace(slot, u))
}
