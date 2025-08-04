// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Conversions between old and new Rust bindings types.
pub trait CompatFrom<T> {
    /// Converts `value` into a value of this type.
    fn compat_from(value: T) -> Self;
}

macro_rules! impl_compat_from_for_primitives {
    ($($ty:ty),* $(,)?) => {
        $(
            impl CompatFrom<$ty> for $ty {
                fn compat_from(value: $ty) -> Self {
                    value
                }
            }
        )*
    };
}

impl_compat_from_for_primitives! {
    i8, i16, i32, i64,
    u8, u16, u32, u64,
    f32, f64,
    bool,
    (),
}

impl<T, U: CompatFrom<T>> CompatFrom<Option<T>> for Option<U> {
    fn compat_from(value: Option<T>) -> Self {
        value.map(U::compat_from)
    }
}

impl<T, U: CompatFrom<T>> CompatFrom<Box<T>> for Box<U> {
    fn compat_from(value: Box<T>) -> Self {
        Box::new(U::compat_from(*value))
    }
}

impl<T, U: CompatFrom<T>, const N: usize> CompatFrom<[T; N]> for [U; N] {
    fn compat_from(value: [T; N]) -> Self {
        value.map(U::compat_from)
    }
}

impl<T, U: CompatFrom<T>> CompatFrom<Vec<T>> for Vec<U> {
    fn compat_from(mut value: Vec<T>) -> Self {
        value.drain(..).map(U::compat_from).collect()
    }
}

impl CompatFrom<String> for String {
    fn compat_from(value: String) -> Self {
        value
    }
}

#[cfg(feature = "fuchsia")]
impl CompatFrom<zx::Handle> for zx::Handle {
    fn compat_from(value: zx::Handle) -> Self {
        value
    }
}

#[cfg(feature = "fuchsia")]
macro_rules! impl_compat_from_for_handle_types {
    ($($name:ident),* $(,)?) => {
        $(
            #[cfg(feature = "fuchsia")]
            impl CompatFrom<zx::$name> for zx::$name {
                fn compat_from(value: zx::$name) -> Self {
                    value
                }
            }
        )*
    };
}

#[cfg(feature = "fuchsia")]
impl_compat_from_for_handle_types! {
    Process,
    Thread,
    Vmo,
    Channel,
    Event,
    Port,
    Interrupt,
    Socket,
    Resource,
    EventPair,
    Job,
    Vmar,
    Fifo,
    Guest,
    Vcpu,
    Timer,
    Iommu,
    Bti,
    Profile,
    Pmt,
    Pager,
    Exception,
    Clock,
    Stream,
    Iob,
}

impl<T: fidl::Timeline> CompatFrom<i64> for ::fidl::Instant<T, ::fidl::NsUnit> {
    fn compat_from(value: i64) -> Self {
        Self::from_nanos(value)
    }
}

impl<T: fidl::Timeline> CompatFrom<::fidl::Instant<T, ::fidl::NsUnit>> for i64 {
    fn compat_from(value: ::fidl::Instant<T, ::fidl::NsUnit>) -> Self {
        value.into_nanos()
    }
}

impl<T: fidl::Timeline> CompatFrom<i64> for fidl::Instant<T, fidl::TicksUnit> {
    fn compat_from(value: i64) -> Self {
        Self::from_raw(value)
    }
}

impl<T: fidl::Timeline> CompatFrom<::fidl::Instant<T, ::fidl::TicksUnit>> for i64 {
    fn compat_from(value: ::fidl::Instant<T, ::fidl::TicksUnit>) -> Self {
        value.into_raw()
    }
}
