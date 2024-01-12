// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use sandbox::{AnyCapability, Capability, ErasedCapability};
use std::borrow::BorrowMut;
use std::fmt::Debug;

/// A capability that holds a string.
#[derive(Capability, Clone, Debug)]
#[capability(as_trait(ReadString))]
struct StringCapability(pub String);

impl Capability for StringCapability {}

trait ReadString {
    fn read(&self) -> String;
}

impl ReadString for StringCapability {
    fn read(&self) -> String {
        self.0.clone()
    }
}

impl From<StringCapability> for fsandbox::Capability {
    fn from(_capability: StringCapability) -> Self {
        unimplemented!()
    }
}

/// A capability that holds an integer.
#[derive(Capability, Clone, Debug)]
struct IntCapability(pub i64);

impl Capability for IntCapability {}

impl From<IntCapability> for fsandbox::Capability {
    fn from(_capability: IntCapability) -> Self {
        unimplemented!()
    }
}

/// Tests that the derived TryFrom<AnyCapability> impl can downcast the capability into Self.
#[test]
fn test_try_from_any_into_self() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    // Downcast back into StringCapability.
    let cap: StringCapability = any.try_into().expect("failed to downcast");

    assert_eq!(cap.0, "hello".to_string());
}

/// Tests the `TryFrom<&AnyCapability>` to reference downcast conversion.
#[test]
fn try_from_any_ref() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    let from: &AnyCapability = &any;
    let to: &StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&mut AnyCapability>` to mut reference downcast conversion.
#[test]
fn try_from_any_mut_ref() {
    let cap = StringCapability("hello".to_string());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut AnyCapability = &mut any;
    let to: &mut StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&dyn ErasedCapability>` to reference downcast conversion.
#[test]
fn try_from_dyn_erased_ref() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    let from: &dyn ErasedCapability = any.as_ref();
    let to: &StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&mut dyn ErasedCapability>` to mut reference downcast conversion.
#[test]
fn try_from_dyn_erased_mut_ref() {
    let cap = StringCapability("hello".to_string());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut dyn ErasedCapability = any.borrow_mut();
    let to: &mut StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}
