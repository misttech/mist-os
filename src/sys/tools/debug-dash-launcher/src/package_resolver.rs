// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fidl_fuchsia_dash as fdash, fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg, tracing::warn};

pub(crate) struct PackageResolver {
    resolver: fpkg::PackageResolverProxy,
}

impl PackageResolver {
    /// Creates a `PackageResolver`.
    /// `fuchsia_pkg_resolver` is the resolver backend to use when resolving "fuchsia-pkg" package
    /// URLs.
    pub(crate) fn new(
        fuchsia_pkg_resolver: fdash::FuchsiaPkgResolver,
    ) -> Result<Self, fdash::LauncherError> {
        // TODO(https://fxbug.dev/329167311) Support bootfs packages
        let suffix = match fuchsia_pkg_resolver {
            fdash::FuchsiaPkgResolver::Base => "base",
            fdash::FuchsiaPkgResolver::Full => "full",
            fdash::FuchsiaPkgResolverUnknown!() => {
                warn!("unknown fuchsia-pkg resolver: {}", fuchsia_pkg_resolver.into_primitive());
                return Err(fdash::LauncherError::PackageResolver);
            }
        };
        let resolver = fuchsia_component::client::connect_to_protocol_at_path::<
            fpkg::PackageResolverMarker,
        >(&format!("/svc/fuchsia.pkg.PackageResolver-{suffix}"))
        .map_err(|_| fdash::LauncherError::PackageResolver)?;
        Ok(Self { resolver })
    }

    /// Resolves `url` and returns a proxy to the package directory.
    pub(crate) async fn resolve(
        &self,
        url: &str,
    ) -> Result<fio::DirectoryProxy, fdash::LauncherError> {
        self.resolve_subpackage(url, &[]).await
    }

    /// Resolves `url` and the chain of `subpackages`, if any, and returns a proxy to the final
    /// package directory.
    pub(crate) async fn resolve_subpackage(
        &self,
        url: &str,
        subpackages: &[String],
    ) -> Result<fio::DirectoryProxy, fdash::LauncherError> {
        let (mut dir, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .map_err(|_| fdash::LauncherError::Internal)?;
        let mut context = self
            .resolver
            .resolve(url, server)
            .await
            .map_err(|_| fdash::LauncherError::Internal)?
            .map_err(|_| fdash::LauncherError::PackageResolver)?;
        for subpackage in subpackages {
            let (sub_dir, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                .map_err(|_| fdash::LauncherError::Internal)?;
            context = self
                .resolver
                .resolve_with_context(subpackage, &context, server)
                .await
                .map_err(|_| fdash::LauncherError::Internal)?
                .map_err(|_| fdash::LauncherError::PackageResolver)?;
            dir = sub_dir;
        }
        Ok(dir)
    }

    #[cfg(test)]
    pub(crate) fn new_test(resolver: fpkg::PackageResolverProxy) -> Self {
        Self { resolver }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_async as fasync;
    use futures::stream::StreamExt as _;

    #[fuchsia::test]
    async fn chain_subpackage_resolves() {
        let (resolver, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fpkg::PackageResolverMarker>().unwrap();
        let resolver = crate::package_resolver::PackageResolver::new_test(resolver);

        // A mock package resolver that records all requests in `requests`.
        let requests = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let requests_clone = requests.clone();
        fasync::Task::local(async move {
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fpkg::PackageResolverRequest::Resolve { package_url, dir: _, responder } => {
                        requests_clone.borrow_mut().push((package_url.clone(), None));
                        responder
                            .send(Ok(&fidl_fuchsia_pkg::ResolutionContext {
                                bytes: package_url.as_bytes().to_vec(),
                            }))
                            .unwrap();
                    }
                    fpkg::PackageResolverRequest::ResolveWithContext {
                        package_url,
                        context,
                        dir: _,
                        responder,
                    } => {
                        requests_clone.borrow_mut().push((package_url.clone(), Some(context)));
                        responder
                            .send(Ok(&fidl_fuchsia_pkg::ResolutionContext {
                                bytes: package_url.as_bytes().to_vec(),
                            }))
                            .unwrap();
                    }
                    req => panic!("unexpected request {req:?}"),
                }
            }
        })
        .detach();

        let _: fio::DirectoryProxy =
            resolver.resolve_subpackage("full-url", &["a".into(), "b".into()]).await.unwrap();

        assert_eq!(
            *requests.borrow_mut(),
            vec![
                ("full-url".into(), None),
                ("a".into(), Some(fpkg::ResolutionContext { bytes: "full-url".into() })),
                ("b".into(), Some(fpkg::ResolutionContext { bytes: "a".into() })),
            ]
        );
    }
}
