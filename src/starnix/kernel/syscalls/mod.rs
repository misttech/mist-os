// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod table;

pub mod misc;
pub mod time;

#[cfg(feature = "syscall_stats")]
pub mod syscall_stats {
    use starnix_syscalls::for_each_syscall;
    use starnix_uapi;

    use fuchsia_inspect as inspect;
    use once_cell::sync::Lazy;
    use paste::paste;

    /// A macro for declaring a SyscallDecl stats property.
    macro_rules! syscall_stats_property {
        ($($name:ident,)*) => {
            paste!{
                $(
                    static [<SYSCALL_ $name:upper _STATS>]: Lazy<inspect::UintProperty> =
                    Lazy::new(|| SYSCALL_STATS_NODE.create_uint(stringify!($name), 0));
                )*
            }
        }
    }

    static SYSCALL_STATS_NODE: Lazy<inspect::Node> =
        Lazy::new(|| inspect::component::inspector().root().create_child("syscall_stats"));
    static SYSCALL_UNKNOWN_STATS: Lazy<inspect::UintProperty> =
        Lazy::new(|| SYSCALL_STATS_NODE.create_uint("<unknown>", 0));

    // Produce each syscall stats property.
    for_each_syscall! {syscall_stats_property}

    macro_rules! syscall_match_stats {
        {$number:ident; $($name:ident,)*} => {
            paste! {
                match $number as u32 {
                    $(starnix_uapi::[<__NR_ $name>] => &[<SYSCALL_ $name:upper _STATS>],)*
                    _ => &SYSCALL_UNKNOWN_STATS,
                }
            }
        }
    }

    pub fn syscall_stats_property(number: u64) -> &'static inspect::UintProperty {
        for_each_syscall! { syscall_match_stats, number }
    }
}
