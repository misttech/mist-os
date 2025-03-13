// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_export]
macro_rules! check_same_layout {
    {
        $type_name1:ty = $type_name2:ty
        {
            $(
                $($field1:ident).+ => $($field2:ident).+
            ),*
            $(,)?
        }
    } => {
        static_assertions::assert_eq_size!($type_name1, $type_name2);
        $(
            static_assertions::const_assert_eq!(
                std::mem::offset_of!($type_name1, $($field1).+),
                std::mem::offset_of!($type_name2, $($field2).+)
            );
        )*
    }
}

#[macro_export]
macro_rules! check_arch_independent_layout {
    {
        $type_name:ident {
            $( $($field:ident).+ ),*
            $(,)?
        }
    }=> {
        $crate::check_same_layout! {
            $crate::$type_name = $crate::arch32::$type_name {
                $(
                    $($field).+ => $($field).+,
                )*
            }
        }
    }
}

#[macro_export]
macro_rules! check_arch_independent_same_layout {
    {
        $type_name1:ty = $type_name2:ident
        {
            $(
                $($field1:ident).+ => $($field2:ident).+
            ),*
            $(,)?
        }
    } => {
        $crate::check_same_layout! {
            $type_name1 = $crate::$type_name2
            {
                $($($field1).+ => $($field2).+,)*
            }
        }
        $crate::check_same_layout! {
            $type_name1 = $crate::arch32::$type_name2
            {
                $($($field1).+ => $($field2).+,)*
            }
        }
    }
}
