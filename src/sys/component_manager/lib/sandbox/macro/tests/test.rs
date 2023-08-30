// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::future::BoxFuture;
use sandbox::{AnyCapability, Capability, Convert, ErasedCapability, Handle, Remote, TryClone};
use std::any::{Any, TypeId};
use std::borrow::BorrowMut;
use std::fmt::Debug;

/// A test-only capability that derives [Capability].
#[derive(Capability, Debug)]
#[capability(try_clone = "err")]
struct TestHandle(zx::Handle);

impl Remote for TestHandle {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.0, None)
    }
}

impl Convert for TestHandle {
    fn try_into_capability(self, type_id: TypeId) -> Result<Box<dyn Any>, ()> {
        assert_eq!(type_id, TypeId::of::<Handle>());
        Ok(Box::new(Handle::from(self.0)))
    }
}

/// Tests that the derived TryFrom<AnyCapability> impl can downcast the capability into Self.
#[test]
fn test_try_from_any_into_self() {
    let event = zx::Event::create();
    let expected_koid = event.get_koid().unwrap();

    let cap = TestHandle(event.into());
    let any: AnyCapability = Box::new(cap);

    // Downcast back into TestHandle.
    let cap: TestHandle = any.try_into().expect("failed to downcast");

    assert_eq!(cap.0.get_koid().unwrap(), expected_koid);
}

/// Tests that the derived TryFrom<AnyCapability> impl can convert the capability into
/// a non-Self type using [Convert.try_into_capability].
#[test]
fn test_try_from_any_convert() {
    let event = zx::Event::create();
    let expected_koid = event.get_koid().unwrap();

    let cap = TestHandle(event.into());
    let any: AnyCapability = Box::new(cap);

    // Convert from type-erased TestHandle into Handle.
    let cap: Handle = any.try_into().expect("failed to convert");

    assert_eq!(cap.as_handle_ref().get_koid().unwrap(), expected_koid);
}

/// A cloneable capability that holds a string.
#[derive(Capability, Clone, Debug)]
#[capability(try_clone = "clone", convert = "to_self_only")]
struct TestCloneable(pub String);

impl Remote for TestCloneable {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        unimplemented!()
    }
}

/// Tests that the derived TryClone impl using `try_clone = "clone"` succeeds.
#[test]
fn test_try_clone() {
    let cap = TestCloneable("hello".to_string());
    let clone = cap.try_clone().expect("failed to clone");
    assert_eq!(clone.0, "hello".to_string());
}

/// Tests that the derived TryClone impl using `try_clone = "none"` returns an error.
#[test]
fn test_try_clone_none() {
    let cap = TestHandle(zx::Handle::invalid());
    assert!(cap.try_clone().is_err());
}

/// Tests that the derived Convert impl using `convert = "try_self_only"` succeeds in converting
/// to the `Self` type.
#[test]
fn test_convert_to_self_only() {
    let cap = TestCloneable("hello".to_string());
    let converted: Box<dyn Any> = cap.try_into_capability(TypeId::of::<TestCloneable>()).unwrap();
    let cap: TestCloneable = *converted.downcast::<TestCloneable>().unwrap();
    assert_eq!(cap.0, "hello".to_string());
}

/// Tests that the derived Convert impl using `convert = "try_self_only"` returns an error
/// when trying to convert to a non-`Self` type.
#[test]
fn test_convert_to_self_only_wrong_type() {
    let cap = TestCloneable("hello".to_string());
    let convert_result = cap.try_into_capability(TypeId::of::<TestHandle>());
    // TestCloneable can only be converted to itself, not to TestHandle.
    assert!(convert_result.is_err());
}

/// Tests the `TryFrom<&AnyCapability>` to reference downcast conversion.
#[test]
fn try_from_any_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let any: AnyCapability = Box::new(cap);

    let from: &AnyCapability = &any;
    let to: &TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&mut AnyCapability>` to mut reference downcast conversion.
#[test]
fn try_from_any_mut_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut AnyCapability = &mut any;
    let to: &mut TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&dyn ErasedCapability>` to reference downcast conversion.
#[test]
fn try_from_dyn_erased_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let any: AnyCapability = Box::new(cap);

    let from: &dyn ErasedCapability = any.as_ref();
    let to: &TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&mut dyn ErasedCapability>` to mut reference downcast conversion.
#[test]
fn try_from_dyn_erased_mut_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut dyn ErasedCapability = any.borrow_mut();
    let to: &mut TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}
