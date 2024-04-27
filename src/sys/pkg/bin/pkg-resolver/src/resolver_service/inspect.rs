// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::inspect_util, fidl_fuchsia_pkg as fpkg, fuchsia_inspect::Node,
    fuchsia_url::AbsolutePackageUrl, fuchsia_zircon as zx, futures::future::BoxFuture,
};

fn now_monotonic_nanos() -> i64 {
    zx::Time::get_monotonic().into_nanos()
}

/// Wraps the Inspect state of package resolves.
#[derive(Debug)]
pub struct ResolverService {
    /// How many times the resolver service has fallen back to the
    /// cache package set due to a remote repository returning NOT_FOUND.
    /// TODO(https://fxbug.dev/42127880): remove this stat when we remove this cache fallback behavior.
    cache_fallbacks_due_to_not_found: inspect_util::Counter,
    active_package_resolves: Node,
    _node: Node,
}

impl ResolverService {
    /// Make a `ResolverService` from an Inspect `Node`.
    pub fn from_node(node: Node) -> Self {
        Self {
            cache_fallbacks_due_to_not_found: inspect_util::Counter::new(
                &node,
                "cache_fallbacks_due_to_not_found",
            ),
            active_package_resolves: node.create_child("active_package_resolves"),
            _node: node,
        }
    }

    /// Increment the count of package resolves that have fallen back to cache packages due to a
    /// remote repository returning NOT_FOUND. This fallback behavior will be removed
    /// TODO(https://fxbug.dev/42127880).
    pub fn cache_fallback_due_to_not_found(&self) {
        self.cache_fallbacks_due_to_not_found.increment();
    }

    /// Add a package to the list of active resolves.
    pub fn resolve(
        &self,
        original_url: &AbsolutePackageUrl,
        gc_protection: fpkg::GcProtection,
    ) -> Package {
        let node = self.active_package_resolves.create_child(original_url.to_string());
        node.record_int("resolve_ts", now_monotonic_nanos());
        node.record_string("gc_protection", format!("{gc_protection:?}"));
        Package { node }
    }

    /// Add a child node for the raw WorkQueue underlying the QueuedResolver.
    pub fn record_raw_queue(
        &self,
        lazy_callback: impl Fn() -> BoxFuture<'static, Result<fuchsia_inspect::Inspector, anyhow::Error>>
            + Send
            + Sync
            + 'static,
    ) {
        let () = self._node.record_lazy_child("raw_queue", lazy_callback);
    }
}

/// A package that is actively being resolved.
pub struct Package {
    node: Node,
}

impl Package {
    /// Export the package's rewritten url.
    pub fn rewritten_url(self, rewritten_url: &AbsolutePackageUrl) -> PackageWithRewrittenUrl {
        self.node.record_string("rewritten_url", rewritten_url.to_string());
        PackageWithRewrittenUrl { _node: self.node }
    }
}

/// A package with a rewritten url that is actively being resolved.
pub struct PackageWithRewrittenUrl {
    _node: Node,
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        diagnostics_assertions::{assert_data_tree, AnyProperty},
        fuchsia_inspect::Inspector,
    };

    #[test]
    fn package_state_progression() {
        let inspector = Inspector::default();

        let resolver_service =
            ResolverService::from_node(inspector.root().create_child("resolver_service"));
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    active_package_resolves: {}
                }
            }
        );

        let package = resolver_service.resolve(
            &"fuchsia-pkg://example.org/name".parse().unwrap(),
            fpkg::GcProtection::Retained,
        );
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    active_package_resolves: {
                        "fuchsia-pkg://example.org/name": {
                            resolve_ts: AnyProperty,
                            gc_protection: "Retained",
                        }
                    }
                }
            }
        );

        let _package =
            package.rewritten_url(&"fuchsia-pkg://rewritten.example.org/name".parse().unwrap());
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    active_package_resolves: {
                        "fuchsia-pkg://example.org/name": {
                            resolve_ts: AnyProperty,
                            gc_protection: "Retained",
                            rewritten_url: "fuchsia-pkg://rewritten.example.org/name",
                        }
                    }
                }
            }
        );
    }

    #[test]
    fn concurrent_resolves() {
        let inspector = Inspector::default();
        let resolver_service =
            ResolverService::from_node(inspector.root().create_child("resolver_service"));

        let _package0 = resolver_service.resolve(
            &"fuchsia-pkg://example.org/name".parse().unwrap(),
            fpkg::GcProtection::Retained,
        );
        let _package1 = resolver_service.resolve(
            &"fuchsia-pkg://example.org/other".parse().unwrap(),
            fpkg::GcProtection::OpenPackageTracking,
        );
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    active_package_resolves: {
                        "fuchsia-pkg://example.org/name": contains {
                            gc_protection: "Retained",
                        },
                        "fuchsia-pkg://example.org/other": contains {
                            gc_protection: "OpenPackageTracking",
                        }
                    }
                }
            }
        );
    }

    #[test]
    fn cache_fallback_due_to_not_found_increments() {
        let inspector = Inspector::default();

        let resolver_service =
            ResolverService::from_node(inspector.root().create_child("resolver_service"));
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    cache_fallbacks_due_to_not_found: 0u64,
                }
            }
        );

        resolver_service.cache_fallback_due_to_not_found();
        assert_data_tree!(
            inspector,
            root: {
                resolver_service: contains {
                    cache_fallbacks_due_to_not_found: 1u64,
                }
            }
        );
    }
}
