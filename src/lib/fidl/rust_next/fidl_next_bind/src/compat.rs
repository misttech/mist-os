// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "fuchsia")]
use fidl_next_codec::CompatFrom;

#[cfg(feature = "fuchsia")]
use crate::Client;
use crate::{ClientEnd, ServerEnd};

macro_rules! impl_compat_from_for_endpoints {
    ($($endpoint:ident),* $(,)?) => { $(
        impl<P1, P2, T> ::fidl_next_codec::CompatFrom<$endpoint<P1, T>> for ::fidl::endpoints::$endpoint<P2>
        where
            ::fidl::Channel: ::fidl_next_codec::CompatFrom<T>,
            P2: ::fidl_next_codec::CompatFrom<P1>,
        {
            fn compat_from(value: $endpoint<P1, T>) -> Self {
                Self::new(::fidl::Channel::compat_from(value.into_untyped()))
            }
        }

        impl<P1, P2, T> ::fidl_next_codec::CompatFrom<::fidl::endpoints::$endpoint<P1>> for $endpoint<P2, T>
        where
            T: ::fidl_next_codec::CompatFrom<::fidl::Channel>,
            P2: ::fidl_next_codec::CompatFrom<P1>,
        {
            fn compat_from(value: ::fidl::endpoints::$endpoint<P1>) ->  Self {
                Self::from_untyped(T::compat_from(value.into_channel()))
            }
        }
    )* };
}

impl_compat_from_for_endpoints!(ClientEnd, ServerEnd);

#[cfg(feature = "fuchsia")]
/// Conversions between old and new Rust protocol bindings.
pub trait ClientCompatFrom<T>: Sized {
    /// Converts `proxy` into a `Client` for this protocol.
    fn client_compat_from(proxy: T) -> Client<Self, zx::Channel>;
}

#[cfg(feature = "fuchsia")]
impl<T, P> CompatFrom<T> for Client<P, zx::Channel>
where
    P: ClientCompatFrom<T>,
{
    fn compat_from(value: T) -> Self {
        P::client_compat_from(value)
    }
}
