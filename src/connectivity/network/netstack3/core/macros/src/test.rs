// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use net_types::ip::{Ip, IpVersion, Ipv4, Ipv6};
use netstack3_macros::instantiate_ip_impl_block;

#[test]
fn instantiate_ip_impl_block() {
    struct Foo {
        ipv4: bool,
        ipv6: bool,
    }

    trait TraitOnIp<I: Ip> {
        fn foo(&mut self);
    }

    #[instantiate_ip_impl_block(I)]
    impl<I> TraitOnIp<I> for Foo {
        fn foo(&mut self) {
            let version = I::VERSION;
            let as_version = <I as Ip>::VERSION;
            assert_eq!(version, as_version);
            let hit = match version {
                IpVersion::V4 => &mut self.ipv4,
                IpVersion::V6 => &mut self.ipv6,
            };
            assert!(!core::mem::replace(hit, true));
        }
    }

    let mut foo = Foo { ipv4: false, ipv6: false };
    TraitOnIp::<Ipv4>::foo(&mut foo);
    TraitOnIp::<Ipv6>::foo(&mut foo);

    let Foo { ipv4, ipv6 } = foo;
    assert!(ipv4);
    assert!(ipv6);
}
