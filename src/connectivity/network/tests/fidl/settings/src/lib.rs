// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use netstack_testing_common::realms::{Netstack3, TestSandboxExt as _};
use netstack_testing_macros::netstack_test;
use {
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_settings as fnet_settings,
};

#[netstack_test]
async fn startup_interface_defaults(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create realm");
    let state = realm.connect_to_protocol::<fnet_settings::StateMarker>().expect("connect state");
    let defaults = state.get_interface_defaults().await.expect("get defaults");
    let interface = realm.join_network(&net, "ep").await.expect("install interface");
    let config = interface
        .control()
        .get_configuration()
        .await
        .expect("get configuration")
        .expect("get config error");
    assert_eq!(config, defaults);
}

#[netstack_test]
async fn interface_defaults(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create realm");
    let state = realm.connect_to_protocol::<fnet_settings::StateMarker>().expect("connect state");
    let defaults = state.get_interface_defaults().await.expect("get defaults");

    let modify_nud = |nud: fnet_interfaces_admin::NudConfiguration| {
        let fnet_interfaces_admin::NudConfiguration {
            max_multicast_solicitations,
            max_unicast_solicitations,
            base_reachable_time,
            __source_breaking,
        } = nud;
        fnet_interfaces_admin::NudConfiguration {
            max_multicast_solicitations: Some(
                max_multicast_solicitations.expect("missing max mcast solicit") + 1,
            ),
            max_unicast_solicitations: Some(
                max_unicast_solicitations.expect("missing max unicast solicit") + 1,
            ),
            base_reachable_time: Some(
                base_reachable_time.expect("missing base reachable time") + 1000,
            ),
            __source_breaking,
        }
    };

    let modify_dad = |dad: fnet_interfaces_admin::DadConfiguration| {
        let fnet_interfaces_admin::DadConfiguration { transmits, __source_breaking } = dad;
        fnet_interfaces_admin::DadConfiguration {
            transmits: Some(transmits.expect("missing transmits") + 1),
            __source_breaking,
        }
    };

    // Modify defaults.
    let fnet_interfaces_admin::Configuration { ipv4, ipv6, __source_breaking } = defaults.clone();
    let fnet_interfaces_admin::Ipv4Configuration {
        unicast_forwarding,
        multicast_forwarding,
        igmp,
        arp,
        __source_breaking,
    } = ipv4.expect("missing ipv4");

    let fnet_interfaces_admin::IgmpConfiguration { version, __source_breaking } =
        igmp.expect("missing igmp");
    let igmp = fnet_interfaces_admin::IgmpConfiguration {
        version: Some(match version.expect("missing IGMP version") {
            fnet_interfaces_admin::IgmpVersion::V3 => fnet_interfaces_admin::IgmpVersion::V1,
            _ => fnet_interfaces_admin::IgmpVersion::V3,
        }),
        __source_breaking: fidl::marker::SourceBreaking,
    };
    let fnet_interfaces_admin::ArpConfiguration { nud, dad, __source_breaking } =
        arp.expect("missing arp");
    let arp = fnet_interfaces_admin::ArpConfiguration {
        nud: Some(modify_nud(nud.expect("missing nud"))),
        dad: Some(modify_dad(dad.expect("missing dad"))),
        __source_breaking: fidl::marker::SourceBreaking,
    };
    let ipv4 = fnet_interfaces_admin::Ipv4Configuration {
        unicast_forwarding: Some(!unicast_forwarding.expect("missing unicast fwd")),
        multicast_forwarding: Some(!multicast_forwarding.expect("missing mcast fwd")),
        igmp: Some(igmp),
        arp: Some(arp),
        __source_breaking: fidl::marker::SourceBreaking,
    };

    let fnet_interfaces_admin::Ipv6Configuration {
        unicast_forwarding,
        multicast_forwarding,
        mld,
        ndp,
        __source_breaking,
    } = ipv6.expect("missing ipv6");
    let fnet_interfaces_admin::MldConfiguration { version, __source_breaking } =
        mld.expect("missing mld");
    let mld = fnet_interfaces_admin::MldConfiguration {
        version: Some(match version.expect("missing mld") {
            fnet_interfaces_admin::MldVersion::V2 => fnet_interfaces_admin::MldVersion::V1,
            _ => fnet_interfaces_admin::MldVersion::V2,
        }),
        __source_breaking,
    };
    let fnet_interfaces_admin::NdpConfiguration { nud, dad, slaac, __source_breaking } =
        ndp.expect("missing ndp");
    let fnet_interfaces_admin::SlaacConfiguration { temporary_address, __source_breaking } =
        slaac.expect("missing slaac");
    let slaac = fnet_interfaces_admin::SlaacConfiguration {
        temporary_address: Some(!temporary_address.expect("missing temp addresses")),
        __source_breaking,
    };
    let ndp = fnet_interfaces_admin::NdpConfiguration {
        nud: Some(modify_nud(nud.expect("missing nud"))),
        dad: Some(modify_dad(dad.expect("missing dad"))),
        slaac: Some(slaac),
        __source_breaking: fidl::marker::SourceBreaking,
    };
    let ipv6 = fnet_interfaces_admin::Ipv6Configuration {
        unicast_forwarding: Some(!unicast_forwarding.expect("missing unicast fwd")),
        multicast_forwarding: Some(!multicast_forwarding.expect("missing mcast fwd")),
        mld: Some(mld),
        ndp: Some(ndp),
        __source_breaking: fidl::marker::SourceBreaking,
    };

    let new_defaults = fnet_interfaces_admin::Configuration {
        ipv4: Some(ipv4),
        ipv6: Some(ipv6),
        __source_breaking: fidl::marker::SourceBreaking,
    };

    let control =
        realm.connect_to_protocol::<fnet_settings::ControlMarker>().expect("connect control");
    let prev = control
        .update_interface_defaults(&new_defaults)
        .await
        .expect("calling update interface defaults")
        .expect("update defaults");
    assert_eq!(prev, defaults);

    let interface = realm.join_network(&net, "ep").await.expect("install interface");
    let config = interface
        .control()
        .get_configuration()
        .await
        .expect("get configuration")
        .expect("get config error");
    assert_eq!(config, new_defaults);

    let defaults = state.get_interface_defaults().await.expect("get defaults");
    assert_eq!(defaults, new_defaults);
}

#[netstack_test]
async fn partial_update_interface(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create realm");
    let state = realm.connect_to_protocol::<fnet_settings::StateMarker>().expect("connect state");
    let defaults = state.get_interface_defaults().await.expect("get defaults");

    let fnet_interfaces_admin::Ipv4Configuration {
        unicast_forwarding, multicast_forwarding, ..
    } = defaults.ipv4.as_ref().unwrap();
    let default_unicast_fwd = unicast_forwarding.unwrap();
    let default_multicast_fwd = multicast_forwarding.unwrap();

    let control =
        realm.connect_to_protocol::<fnet_settings::ControlMarker>().expect("connect control");
    let update = fnet_interfaces_admin::Configuration {
        ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
            // Apply the same value to unicast, and a different value to
            // multicast to prove we get the previous value returns either way.
            unicast_forwarding: Some(default_unicast_fwd),
            multicast_forwarding: Some(!default_multicast_fwd),
            ..Default::default()
        }),
        ..Default::default()
    };
    let prev = control
        .update_interface_defaults(&update)
        .await
        .expect("calling update interface defaults")
        .expect("update defaults");

    // The touched values return their previous configuration.
    let expect = fnet_interfaces_admin::Configuration {
        ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
            unicast_forwarding: Some(default_unicast_fwd),
            multicast_forwarding: Some(default_multicast_fwd),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(prev, expect);
}
