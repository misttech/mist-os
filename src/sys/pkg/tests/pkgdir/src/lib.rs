// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]

use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg_test::{RealmFactoryMarker, RealmOptions},
    fio::DirectoryMarker,
    fuchsia_component::client::connect_to_protocol,
};

mod directory;
mod file;
mod node;

fn repeat_by_n(seed: char, n: usize) -> String {
    std::iter::repeat(seed).take(n).collect()
}

async fn dirs_to_test() -> impl Iterator<Item = PackageSource> {
    // Bind to parent to ensure driver test realm is started
    let _ = connect_to_protocol::<fcomponent::BinderMarker>().unwrap();
    let realm_factory =
        connect_to_protocol::<RealmFactoryMarker>().expect("connect to realm_factory");
    let (directory, server_end) =
        fidl::endpoints::create_proxy::<DirectoryMarker>().expect("create proxy");
    realm_factory
        .create_realm(RealmOptions { pkg_directory_server: Some(server_end), ..Default::default() })
        .await
        .expect("create_realm fidl failed")
        .expect("create_realm failed");

    let connect = || async move { PackageSource { dir: directory } };
    IntoIterator::into_iter([connect().await])
}

struct PackageSource {
    dir: fio::DirectoryProxy,
}

macro_rules! flag_list {
    [$($flag:ident),* $(,)?] => {
        [
            $((fio::$flag, stringify!($flag))),*
        ]
    };
}

// modes in same order as they appear in fuchsia.io in an attempt to make it
// easier to keep this list up to date. Although if this list gets out of date
// it's not the end of the world, the debug printer just won't know how to
// decode them and will octal format the not-decoded flags.
const MODE_TYPES: &[(u32, &str)] =
    &flag_list![MODE_TYPE_DIRECTORY, MODE_TYPE_BLOCK_DEVICE, MODE_TYPE_FILE, MODE_TYPE_SERVICE,];

#[derive(PartialEq, Eq)]
struct Mode(u32);

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut flags = self.0;
        let flag_strings = MODE_TYPES.iter().filter_map(|&flag| {
            if self.0 & flag.0 == flag.0 {
                flags &= !flag.0;
                Some(flag.1)
            } else {
                None
            }
        });
        let mut first = true;
        for flag in flag_strings {
            if !first {
                write!(f, " | ")?;
            }
            first = false;
            write!(f, "{flag}")?;
        }
        if flags != 0 {
            if !first {
                write!(f, " | ")?;
            }
            first = false;
            write!(f, "{flags:#o}")?;
        }
        if first {
            write!(f, "0")?;
        }

        Ok(())
    }
}
