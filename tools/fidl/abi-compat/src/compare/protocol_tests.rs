// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::problems::ProblemPattern;
use super::problems::StringPattern::*;
use super::test::*;

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

mod method {
    use super::*;

    /// Helper to generate protocols with changing methods.
    fn protocol(discoverable: &str, change: &str, openness: &str, flexibility: &str) -> String {
        let discoverable = if discoverable == "" {
            "@discoverable".to_string()
        } else {
            format!("@discoverable({discoverable})")
        };

        "
        DISCOVERABLE
        OPENNESS protocol Foo {
            strict Bar() -> ();
            @available(CHANGE)
            FLEXIBILITY Baz() -> ();
        };"
        .replace("DISCOVERABLE", &discoverable)
        .replace("OPENNESS", openness)
        .replace("CHANGE", change)
        .replace("FLEXIBILITY", flexibility)
    }

    #[test]
    fn server_platform_add_remove_method() {
        // With server=platform it's safe to add and remove methods because the
        // platform will support the superset of all stable and unstable levels'
        // methods.
        for change in vec!["added=2", "added=HEAD", "removed=2", "removed=HEAD"] {
            for (openness, flexibility) in
                vec![("closed", "strict"), ("open", "strict"), ("open", "flexible")]
            {
                assert!(compare_fidl_library(
                    Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                    protocol("server=\"platform\"", change, openness, flexibility)
                )
                .is_compatible());
            }
        }
    }

    #[test]
    fn server_external_add_method() {
        let changes = vec!["added=2", "added=HEAD"];
        let discoverable = "server=\"external\"";
        // For strict methods this should fail.
        for change in &changes {
            for (openness, flexibility) in vec![("closed", "strict"), ("open", "strict")] {
                assert!(compare_fidl_library(
                    Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                    protocol(discoverable, change, openness, flexibility)
                )
                .has_problems(vec![
                    ProblemPattern::protocol("PLATFORM", "1").message(Contains("missing method"))
                ]));
            }
        }
        // For flexible methods this should succeed.
        for change in changes {
            for (openness, flexibility) in vec![("open", "flexible")] {
                assert!(compare_fidl_library(
                    Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                    protocol(discoverable, change, openness, flexibility)
                )
                .is_compatible());
            }
        }
    }

    #[test]
    fn remove_method() {
        // It's always safe to remove methods because the platform will support the
        // superset of all stable and unstable levels' methods.

        let discoverables = ["", "server=\"platform\"", "server=\"external\""];

        for discoverable in discoverables {
            for change in vec!["removed=2", "removed=HEAD"] {
                for (openness, flexibility) in
                    vec![("closed", "strict"), ("open", "strict"), ("open", "flexible")]
                {
                    assert!(compare_fidl_library(
                        Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                        protocol(discoverable, change, openness, flexibility)
                    )
                    .is_compatible());
                }
            }
        }
    }

    #[test]
    fn server_anywhere_add_method() {
        let changes = vec!["added=2", "added=HEAD"];
        let discoverable = "name=\"fuchsia.example.Foo\"";

        // For strict methods this should fail.
        for change in &changes {
            for (openness, flexibility) in vec![("closed", "strict"), ("open", "strict")] {
                assert!(compare_fidl_library(
                    Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                    protocol(discoverable, change, openness, flexibility)
                )
                .has_problems(vec![
                    ProblemPattern::protocol("PLATFORM", "1").message(Contains("missing method"))
                ]));
            }
        }
        // For flexible methods this should succeed.
        for change in changes {
            for (openness, flexibility) in vec![("open", "flexible")] {
                assert!(compare_fidl_library(
                    Versions { external: "1", platform: "1,2,NEXT,HEAD" },
                    protocol(discoverable, change, openness, flexibility)
                )
                .is_compatible());
            }
        }
    }
}
