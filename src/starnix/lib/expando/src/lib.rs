// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_sync::Mutex;
use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::marker::{Send, Sync};
use std::ops::Deref;
use std::sync::Arc;

/// A spot in an `Expando`.
///
/// Holds a value of type `Arc<T>`.
#[derive(Debug)]
struct ExpandoSlot {
    value: Arc<dyn Any + Send + Sync>,
}

impl ExpandoSlot {
    fn new(value: Arc<dyn Any + Send + Sync>) -> Self {
        ExpandoSlot { value }
    }

    fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.value.clone().downcast::<T>().ok()
    }
}

/// A lazy collection of values of every type.
///
/// An Expando contains a single instance of every type. The values are instantiated lazily
/// when accessed. Useful for letting modules add their own state to context objects without
/// requiring the context object itself to know about the types in every module.
///
/// Typically the type a module uses in the Expando will be private to that module, which lets
/// the module know that no other code is accessing its slot on the expando.
#[derive(Debug, Default)]
pub struct Expando {
    properties: Mutex<BTreeMap<TypeId, ExpandoSlot>>,
}

impl Expando {
    /// Get the slot in the expando associated with the given type.
    ///
    /// The slot is added to the expando lazily but the same instance is returned every time the
    /// expando is queried for the same type.
    pub fn get<T: Any + Send + Sync + Default + 'static>(&self) -> Arc<T> {
        let mut properties = self.properties.lock();
        let type_id = TypeId::of::<T>();
        let slot =
            properties.entry(type_id).or_insert_with(|| ExpandoSlot::new(Arc::new(T::default())));
        assert_eq!(type_id, slot.value.deref().type_id());
        slot.downcast().expect("downcast of expando slot was successful")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct MyStruct {
        counter: Mutex<i32>,
    }

    #[test]
    fn basic_test() {
        let expando = Expando::default();
        let first = expando.get::<MyStruct>();
        assert_eq!(*first.counter.lock(), 0);
        *first.counter.lock() += 1;
        let second = expando.get::<MyStruct>();
        assert_eq!(*second.counter.lock(), 1);
    }
}
