// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef};
use once_cell::sync::Lazy;
use tracing::{Callsite, Metadata};
use tracing_core::{field, subscriber::Interest};

pub static PLACEHOLDER_TEXT: Lazy<String> = Lazy::new(|| "x".repeat(32000));
pub static PROCESS_ID: Lazy<zx::Koid> =
    Lazy::new(|| fuchsia_runtime::process_self().get_koid().unwrap());
pub static THREAD_ID: Lazy<zx::Koid> =
    Lazy::new(|| fuchsia_runtime::thread_self().get_koid().unwrap());

pub struct NullCallsite;
pub static NULL_CALLSITE: NullCallsite = NullCallsite;
impl Callsite for NullCallsite {
    fn set_interest(&self, _: Interest) {
        unreachable!("unused by the encoder")
    }

    fn metadata(&self) -> &Metadata<'_> {
        unreachable!("unused by the encoder")
    }
}

macro_rules! len {
    () => (0usize);
    ($head:ident $($tail:ident)*) => (1usize + crate::common::len!($($tail)*));
}

type ValueSetItem<'a> = (&'a field::Field, Option<&'a (dyn field::Value + 'a)>);

// TODO: this will be much cleaner when Array zip is stable.
// https://github.com/rust-lang/rust/issues/80094
pub fn make_value_set<'a, const N: usize>(
    fields: &'a [field::Field; N],
    values: &[&'a (dyn field::Value + 'a); N],
) -> [ValueSetItem<'a>; N] {
    // Safety: assume_init is safe because the type is MaybeUninit which
    // doesn't need to be initialized.
    // Use uninit_array when stabilized.
    // https://doc.rust-lang.org/stable/src/core/mem/maybe_uninit.rs.html#350
    let mut data: [std::mem::MaybeUninit<ValueSetItem<'a>>; N] =
        unsafe { std::mem::MaybeUninit::uninit().assume_init() };
    for (i, slot) in data.iter_mut().enumerate() {
        slot.write((&fields[i], Some(values[i])));
    }
    // See https://github.com/rust-lang/rust/issues/61956 for why we can't use transmute.
    // Safety: everything has been initialized now.
    unsafe { std::mem::transmute_copy::<_, [ValueSetItem<'a>; N]>(&data) }
}

// Generates the metadata required to create a tracing::Event with the lifetimes that it expects.
macro_rules! make_event_metadata {
    ($($key:ident: $value:expr),*) => {{
        static METADATA: tracing::Metadata<'static> = tracing::Metadata::new(
            /* name */ "benching",
            /* target */ "bench",
            tracing::Level::INFO,
            Some(file!()),
            Some(line!()),
            Some(module_path!()),
            tracing_core::field::FieldSet::new(
                &[$( stringify!($key),)*],
                tracing_core::identify_callsite!(&crate::common::NULL_CALLSITE),
            ),
            tracing_core::Kind::EVENT,
        );
        const N : usize = crate::common::len!($($key)*);
        let fields : [tracing_core::field::Field; N] = [
            $(METADATA.fields().field(stringify!($key)).unwrap(),)*
        ];
        let values : [&dyn tracing_core::field::Value; N] = [
            $((&$value as &dyn tracing_core::field::Value),)*
        ];
        (&METADATA, fields, values)
    }};
}

pub(crate) use {len, make_event_metadata};
