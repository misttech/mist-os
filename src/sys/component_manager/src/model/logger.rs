// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::instance::ResolvedInstanceState;
use crate::model::component::ComponentInstance;
use diagnostics_log::{Publisher, PublisherOptions};
use fidl::endpoints;
use fidl::endpoints::DiscoverableProtocolMarker;
use log::Log;
use moniker::Moniker;
use routing::DictExt;
use sandbox::{Capability, RemotableCapability};
use std::collections::LinkedList;
use std::sync::{Arc, LazyLock, Mutex};
use vfs::directory::entry::OpenRequest;
use vfs::ToObjectRequest;
use {fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger};

const CACHE_SIZE: usize = 10;
static LOGGER_CACHE: LazyLock<Mutex<LoggerCache>> =
    LazyLock::new(|| Mutex::new(LoggerCache { list: LinkedList::new() }));

pub struct LoggerCache {
    list: LinkedList<(Moniker, Arc<Publisher>)>,
}

impl LoggerCache {
    /// Tries to logs on behalf of the component and falls back to the global log on failure.
    pub fn log(
        moniker: &Moniker,
        resolved_instance_state: &ResolvedInstanceState,
        record: &log::Record,
    ) {
        if !Self::try_attributed_log(moniker, resolved_instance_state, record) {
            log::logger().log(record);
        }
    }

    /// Purges the cache for the component.
    pub fn purge(component: &ComponentInstance) {
        LOGGER_CACHE.lock().unwrap().list.extract_if(|e| &e.0 == &component.moniker).next();
    }

    /// Tries to logs on behalf of the component.
    pub fn try_attributed_log(
        moniker: &Moniker,
        resolved_instance_state: &ResolvedInstanceState,
        record: &log::Record,
    ) -> bool {
        let mut cache = LOGGER_CACHE.lock().unwrap();
        if let Some(element) = cache.list.extract_if(|e| &e.0 == moniker).next() {
            element.1.log(record);
            cache.list.push_front(element);
            return true;
        }

        // Check that the component includes the logsink capability before logging on its behalf.
        let decl = &resolved_instance_state.resolved_component.decl;
        let Some(decl) = get_logsink_decl(&decl) else {
            return false;
        };
        let program_input_dict = resolved_instance_state.sandbox.program_input.namespace();
        let Some(Capability::ConnectorRouter(router)) =
            program_input_dict.get_capability(&decl.target_path)
        else {
            return false;
        };

        let scope = resolved_instance_state.execution_scope.clone();
        let Ok(dir_entry) = router.try_into_directory_entry(scope.clone()) else {
            return false;
        };
        let (logsink, server) = endpoints::create_endpoints::<flogger::LogSinkMarker>();
        const FLAGS: fio::Flags = fio::Flags::PROTOCOL_SERVICE;
        FLAGS.to_object_request(server.into_channel()).handle(|object_request| {
            dir_entry.clone().open_entry(OpenRequest::new(
                scope,
                FLAGS,
                vfs::path::Path::dot(),
                object_request,
            ))
        });

        let Ok(publisher) = Publisher::new(PublisherOptions::empty().use_log_sink(logsink)) else {
            return false;
        };

        publisher.log(record);
        cache.list.push_front((moniker.clone(), Arc::new(publisher)));

        if cache.list.len() > CACHE_SIZE {
            cache.list.pop_back();
        }

        true
    }
}

/// Returns the UseProtocolDecl for the LogSink protocol, if any.
fn get_logsink_decl<'a>(decl: &'a cm_rust::ComponentDecl) -> Option<&'a cm_rust::UseProtocolDecl> {
    decl.uses.iter().find_map(|use_| match use_ {
        cm_rust::UseDecl::Protocol(decl) => {
            (decl.source_name == flogger::LogSinkMarker::PROTOCOL_NAME).then_some(decl)
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::{LoggerCache, CACHE_SIZE, LOGGER_CACHE};
    use crate::model::component::instance::ResolvedInstanceState;
    use crate::model::component::{Component, ComponentInstance, WeakExtendedInstance};
    use crate::model::context::ModelContext;
    use crate::model::environment::Environment;
    use cm_rust::{UseDecl, UseProtocolDecl, UseSource};
    use cm_rust_testing::ComponentDeclBuilder;
    use cm_types::{BoundedName, Path, Url};
    use fidl::endpoints::DiscoverableProtocolMarker;
    use fidl_fuchsia_component_decl::{self as fdecl, OnTerminate};
    use fidl_fuchsia_logger as flogger;
    use hooks::Hooks;
    use routing::bedrock::structured_dict::ComponentInput;
    use routing::resolving::ComponentAddress;
    use sandbox::{Capability, Connector, Receiver, Router};
    use std::str::FromStr;
    use std::sync::{Arc, Weak};

    const LOG_SINK_PROTOCOL: &str = flogger::LogSinkMarker::PROTOCOL_NAME;

    async fn new_instance_with_name(
        name: &str,
        decl: cm_rust::ComponentDecl,
        parent_capabilities: ComponentInput,
    ) -> (Arc<ComponentInstance>, ResolvedInstanceState) {
        let url: Url = "test:///foo".parse().unwrap();
        let instance = ComponentInstance::new(
            ComponentInput::default(),
            Arc::new(Environment::empty()),
            name.try_into().unwrap(),
            1,
            url.clone(),
            fdecl::StartupMode::Lazy,
            OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::AboveRoot(Weak::new()),
            Arc::new(Hooks::new()),
            false,
        )
        .await;
        let resolved_component = Component {
            resolved_url: "".to_string(),
            context_to_resolve_children: None,
            decl,
            package: None,
            config: None,
            abi_revision: None,
        };
        let resolved_state = ResolvedInstanceState::new(
            &instance,
            resolved_component,
            ComponentAddress::from_absolute_url(&url).unwrap(),
            Default::default(),
            parent_capabilities,
        )
        .await
        .unwrap();
        (instance, resolved_state)
    }

    async fn new_instance_with_valid_logsink(
        name: &str,
    ) -> (Arc<ComponentInstance>, ResolvedInstanceState, Receiver) {
        let target_path: Path = format!("/svc/{LOG_SINK_PROTOCOL}").parse().unwrap();
        let decl = ComponentDeclBuilder::new_empty_component()
            .use_(UseDecl::Protocol(UseProtocolDecl {
                source: UseSource::Parent,
                source_name: LOG_SINK_PROTOCOL.parse().unwrap(),
                source_dictionary: Default::default(),
                target_path: target_path.clone(),
                dependency_type: cm_rust::DependencyType::Strong,
                availability: cm_rust::Availability::Required,
            }))
            .build();
        let input = ComponentInput::default();
        let (rx, connector) = Connector::new();
        input
            .insert_capability(
                &BoundedName::from_str(LOG_SINK_PROTOCOL).unwrap(),
                Capability::ConnectorRouter(Router::new_ok(connector)),
            )
            .unwrap();
        let (instance, resolved_state) = new_instance_with_name(name, decl, input).await;
        (instance, resolved_state, rx)
    }

    async fn new_instance_without_logsink_decl() -> (Arc<ComponentInstance>, ResolvedInstanceState)
    {
        let decl = ComponentDeclBuilder::new_empty_component().build();
        new_instance_with_name("child", decl, ComponentInput::default()).await
    }

    fn new_record() -> log::Record<'static> {
        log::Record::builder().args(format_args!("foo")).build()
    }

    #[fuchsia::test]
    async fn try_attributed_log_with_logsink() {
        let (instance, resolved_instance, rx) = new_instance_with_valid_logsink("foo").await;
        assert!(LoggerCache::try_attributed_log(
            &instance.moniker,
            &resolved_instance,
            &new_record(),
        ));
        assert!(rx.receive().await.is_some());
    }

    #[fuchsia::test]
    async fn try_attributed_log_without_logsink_decl() {
        let (instance, resolved_instance) = new_instance_without_logsink_decl().await;
        assert!(!LoggerCache::try_attributed_log(
            &instance.moniker,
            &resolved_instance,
            &new_record(),
        ));
    }

    #[fuchsia::test]
    async fn log_twice_should_hit_cache() {
        let (instance, resolved_instance, rx) = new_instance_with_valid_logsink("foo").await;
        let record = new_record();

        // First log, populates cache.
        assert!(LoggerCache::try_attributed_log(&instance.moniker, &resolved_instance, &record));
        assert!(rx.receive().await.is_some());
        assert_eq!(LOGGER_CACHE.lock().unwrap().list.len(), 1);

        // Second log, should hit cache. Use an instance that would fail.
        let (_, resolved_instance_fail) = new_instance_without_logsink_decl().await;
        assert!(LoggerCache::try_attributed_log(
            &instance.moniker,
            &resolved_instance_fail,
            &record
        ));
        assert_eq!(LOGGER_CACHE.lock().unwrap().list.len(), 1);
    }

    #[fuchsia::test]
    async fn cache_eviction() {
        let record = new_record();

        let mut instances = Vec::new();
        for i in 0..CACHE_SIZE {
            let (instance, resolved_instance, rx) =
                new_instance_with_valid_logsink(&format!("child-{}", i)).await;
            assert!(LoggerCache::try_attributed_log(
                &instance.moniker,
                &resolved_instance,
                &record
            ));
            assert!(rx.receive().await.is_some());
            instances.push(instance);
        }
        assert_eq!(LOGGER_CACHE.lock().unwrap().list.len(), CACHE_SIZE);

        // Log one more time, should evict the first one.
        let (instance, resolved_instance, rx) = new_instance_with_valid_logsink("new-child").await;
        assert!(LoggerCache::try_attributed_log(&instance.moniker, &resolved_instance, &record));
        assert!(rx.receive().await.is_some());

        let cache = LOGGER_CACHE.lock().unwrap();
        assert_eq!(cache.list.len(), CACHE_SIZE);
        assert!(!cache.list.iter().any(|(m, _)| m == &instances[0].moniker));
        assert!(cache.list.iter().any(|(m, _)| m == &instance.moniker));
    }

    #[fuchsia::test]
    async fn purge_removes_from_cache() {
        let (instance, resolved_instance, rx) = new_instance_with_valid_logsink("foo").await;

        // Log to populate the cache.
        assert!(LoggerCache::try_attributed_log(
            &instance.moniker,
            &resolved_instance,
            &new_record()
        ));
        assert!(rx.receive().await.is_some());
        assert_eq!(LOGGER_CACHE.lock().unwrap().list.len(), 1);

        // Purge and check that the cache is empty.
        LoggerCache::purge(&instance);
        assert_eq!(LOGGER_CACHE.lock().unwrap().list.len(), 0);
    }
}
