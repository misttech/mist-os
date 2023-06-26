// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        above_root_capabilities::AboveRootCapabilitiesForTest,
        constants,
        debug_data_processor::{DebugDataDirectory, DebugDataProcessor},
        error::TestManagerError,
        facet,
        offers::map_offers,
        running_suite::{enumerate_test_cases, RunningSuite},
        self_diagnostics::RootDiagnosticNode,
        test_suite::{Suite, SuiteRealm, TestRunBuilder},
    },
    fidl::endpoints::ControlHandle,
    fidl_fuchsia_component_resolution::ResolverProxy,
    fidl_fuchsia_test_manager as ftest_manager,
    fidl_fuchsia_test_manager::{QueryEnumerateInRealmResponder, QueryEnumerateResponder},
    ftest_manager::LaunchError,
    fuchsia_async::{self as fasync},
    fuchsia_zircon as zx,
    futures::prelude::*,
    std::sync::Arc,
    tracing::warn,
};

/// Start test manager and serve it over `stream`.
pub async fn run_test_manager(
    mut stream: ftest_manager::RunBuilderRequestStream,
    resolver: Arc<ResolverProxy>,
    above_root_capabilities_for_test: Arc<AboveRootCapabilitiesForTest>,
    root_diagnostics: &RootDiagnosticNode,
) -> Result<(), TestManagerError> {
    let mut builder = TestRunBuilder { suites: vec![] };
    let mut scheduling_options: Option<ftest_manager::SchedulingOptions> = None;
    while let Some(event) = stream.try_next().await.map_err(TestManagerError::Stream)? {
        match event {
            ftest_manager::RunBuilderRequest::AddSuite {
                test_url,
                options,
                controller,
                control_handle,
            } => {
                let controller = match controller.into_stream() {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(
                            "Cannot add suite {}, invalid controller. Closing connection. error: {}",
                            test_url,e
                        );
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_HANDLE);
                        break;
                    }
                };

                builder.suites.push(Suite {
                    realm: None,
                    test_url,
                    options,
                    controller,
                    resolver: resolver.clone(),
                    above_root_capabilities_for_test: above_root_capabilities_for_test.clone(),
                    facets: facet::ResolveStatus::Unresolved,
                });
            }
            ftest_manager::RunBuilderRequest::AddSuiteInRealm {
                realm,
                offers,
                test_collection,
                test_url,
                options,
                controller,
                control_handle,
            } => {
                let realm_proxy = match realm.into_proxy() {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "Cannot add suite {}, invalid realm. Closing connection. error: {}",
                            test_url, e
                        );
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_HANDLE);
                        break;
                    }
                };
                let controller = match controller.into_stream() {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(
                            "Cannot add suite {}, invalid controller. Closing connection. error: {}",
                            test_url,e
                        );
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_HANDLE);
                        break;
                    }
                };
                let offers = match map_offers(offers) {
                    Ok(offers) => offers,
                    Err(e) => {
                        warn!("Cannot add suite {}, invalid offers. error: {}", test_url, e);
                        control_handle.shutdown_with_epitaph(zx::Status::INVALID_ARGS);
                        break;
                    }
                };

                builder.suites.push(Suite {
                    realm: SuiteRealm { realm_proxy, offers, test_collection }.into(),
                    test_url,
                    options,
                    controller,
                    resolver: resolver.clone(),
                    above_root_capabilities_for_test: above_root_capabilities_for_test.clone(),
                    facets: facet::ResolveStatus::Unresolved,
                });
            }
            ftest_manager::RunBuilderRequest::WithSchedulingOptions { options, .. } => {
                scheduling_options = Some(options);
            }
            ftest_manager::RunBuilderRequest::Build { controller, control_handle } => {
                let controller = match controller.into_stream() {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("Invalid builder controller. Closing connection. error: {}", e);
                        control_handle.shutdown_with_epitaph(zx::Status::BAD_HANDLE);
                        break;
                    }
                };

                let persist_diagnostics =
                    match scheduling_options.as_ref().map(|options| options.max_parallel_suites) {
                        Some(Some(_)) => true,
                        Some(None) | None => false,
                    };
                let diagnostics = match persist_diagnostics {
                    true => root_diagnostics.persistent_child(),
                    false => root_diagnostics.child(),
                };

                builder.run(controller, diagnostics, scheduling_options).await;
                // clients should reconnect to run new tests.
                break;
            }
        }
    }
    Ok(())
}

enum QueryResponder {
    Enumerate(QueryEnumerateResponder),
    EnumerateInRealm(QueryEnumerateInRealmResponder),
}

impl QueryResponder {
    fn send(self, result: Result<(), LaunchError>) -> Result<(), fidl::Error> {
        match self {
            QueryResponder::Enumerate(responder) => responder.send(result),
            QueryResponder::EnumerateInRealm(responder) => responder.send(result),
        }
    }
}

/// Start test manager and serve it over `stream`.
pub async fn run_test_manager_query_server(
    mut stream: ftest_manager::QueryRequestStream,
    resolver: Arc<ResolverProxy>,
    above_root_capabilities_for_test: Arc<AboveRootCapabilitiesForTest>,
    root_diagnostics: &RootDiagnosticNode,
) -> Result<(), TestManagerError> {
    while let Some(event) = stream.try_next().await.map_err(TestManagerError::Stream)? {
        let (test_url, iterator, realm, responder) = match event {
            ftest_manager::QueryRequest::Enumerate { test_url, iterator, responder } => {
                (test_url, iterator, None, QueryResponder::Enumerate(responder))
            }
            ftest_manager::QueryRequest::EnumerateInRealm {
                test_url,
                realm,
                offers,
                test_collection,
                iterator,
                responder,
            } => {
                let realm_proxy = match realm.into_proxy() {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "Cannot add suite {}, invalid realm. Closing connection. error: {}",
                            test_url, e
                        );
                        let _ = responder.send(Err(LaunchError::InvalidArgs));
                        break;
                    }
                };
                let offers = match map_offers(offers) {
                    Ok(offers) => offers,
                    Err(e) => {
                        warn!("Cannot add suite {}, invalid offers. error: {}", test_url, e);
                        let _ = responder.send(Err(LaunchError::InvalidArgs));
                        break;
                    }
                };
                (
                    test_url,
                    iterator,
                    SuiteRealm { realm_proxy, offers, test_collection }.into(),
                    QueryResponder::EnumerateInRealm(responder),
                )
            }
        };
        let mut iterator = match iterator.into_stream() {
            Ok(c) => c,
            Err(e) => {
                warn!("Cannot query test, invalid iterator {}: {}", test_url, e);
                let _ = responder.send(Err(LaunchError::InvalidArgs));
                break;
            }
        };
        let (_processor, sender) = DebugDataProcessor::new(DebugDataDirectory::Isolated {
            parent: constants::ISOLATED_TMP,
        });
        let diagnostics = root_diagnostics.child();
        let launch_fut =
            facet::get_suite_facets(test_url.clone(), resolver.clone()).and_then(|facets| {
                RunningSuite::launch(
                    &test_url,
                    facets,
                    resolver.clone(),
                    above_root_capabilities_for_test.clone(),
                    sender,
                    &diagnostics,
                    &realm,
                )
            });
        match launch_fut.await {
            Ok(suite_instance) => {
                let suite = match suite_instance.connect_to_suite() {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        let _ = responder.send(Err(e.into()));
                        continue;
                    }
                };
                let enumeration_result = enumerate_test_cases(&suite, None).await;
                let t = fasync::Task::spawn(suite_instance.destroy());
                match enumeration_result {
                    Ok(invocations) => {
                        const NAMES_CHUNK: usize = 50;
                        let mut names = Vec::with_capacity(invocations.len());
                        if let Ok(_) = invocations.into_iter().try_for_each(|i| match i.name {
                            Some(name) => {
                                names.push(name);
                                Ok(())
                            }
                            None => {
                                warn!("no name for a invocation in {}", test_url);
                                Err(())
                            }
                        }) {
                            let _ = responder.send(Ok(()));
                            let mut names = names.chunks(NAMES_CHUNK);
                            while let Ok(Some(request)) = iterator.try_next().await {
                                match request {
                                    ftest_manager::CaseIteratorRequest::GetNext { responder } => {
                                        match names.next() {
                                            Some(names) => {
                                                let _ = responder.send(
                                                    &names
                                                        .into_iter()
                                                        .map(|s| ftest_manager::Case {
                                                            name: Some(s.into()),
                                                            ..Default::default()
                                                        })
                                                        .collect::<Vec<_>>(),
                                                );
                                            }
                                            None => {
                                                let _ = responder.send(&[]);
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            let _ = responder.send(Err(LaunchError::CaseEnumeration));
                        }
                    }
                    Err(err) => {
                        warn!(?err, "cannot enumerate tests for {}", test_url);
                        let _ = responder.send(Err(LaunchError::CaseEnumeration));
                    }
                }
                if let Err(err) = t.await {
                    warn!(?err, "Error destroying test realm for {}", test_url);
                }
            }
            Err(e) => {
                let _ = responder.send(Err(e.into()));
            }
        }
    }
    Ok(())
}
