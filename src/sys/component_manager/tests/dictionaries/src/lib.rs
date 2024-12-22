// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fidl_test_components as ftest;
use fuchsia_component::client;

#[fuchsia::test]
async fn protocols() {
    fn path(letter: &str) -> String {
        format!("/svc/fidl.test.components.Trigger-{letter}")
    }

    // See the test's cml to understand how this exercises dictionary routing.
    {
        let trigger =
            client::connect_to_protocol_at_path::<ftest::TriggerMarker>(&path("a")).unwrap();
        let out = trigger.run().await.unwrap();
        assert_eq!(&out, "Triggered a");
    }
    {
        let trigger =
            client::connect_to_protocol_at_path::<ftest::TriggerMarker>(&path("b")).unwrap();
        let out = trigger.run().await.unwrap();
        assert_eq!(&out, "Triggered b");
    }
    {
        let trigger =
            client::connect_to_protocol_at_path::<ftest::TriggerMarker>(&path("c")).unwrap();
        let out = trigger.run().await.unwrap();
        assert_eq!(&out, "Triggered c");
    }
    {
        let trigger =
            client::connect_to_protocol_at_path::<ftest::TriggerMarker>(&path("d")).unwrap();
        let out = trigger.run().await.unwrap();
        assert_eq!(&out, "Triggered d");
    }
}

// TODO(https://fxbug.dev/383601465): Add tests for more capability types.
