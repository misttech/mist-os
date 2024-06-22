// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::test::compare_fidl_protocol;

#[test]
fn protocol_openness() {
    assert!(compare_fidl_protocol(
        "Foo",
        "
    @available(replaced=2)
    @discoverable
    closed protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };

    @available(added=2)
    @discoverable
    open protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };
    "
    )
    .is_compatible());

    assert!(compare_fidl_protocol(
        "Foo",
        "
    @available(replaced=2)
    @discoverable
    ajar protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };

    @available(added=2)
    @discoverable
    open protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };
    "
    )
    .is_compatible());

    assert!(compare_fidl_protocol(
        "Foo",
        "
    @available(replaced=2)
    @discoverable
    closed protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };

    @available(added=2)
    @discoverable
    ajar protocol Foo {
        strict OneWay();
        strict TwoWay() -> ();
        strict -> Event();
    };
    "
    )
    .is_compatible());
}
