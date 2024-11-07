// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::agent::{
    AgentCreator, AgentError, Context, Invocation, InvocationResult, Lifespan,
    Payload as AgentPayload,
};
use crate::base::{Dependency, Entity, SettingType};
use crate::event::{Event, Payload as EventPayload};
use crate::ingress::{fidl, registration};
use crate::inspect::config_logger::InspectConfigLogger;
use crate::job::source::Error;
use crate::job::{self, Job};
use crate::light::build_light_default_settings;
use crate::message::base::{Audience, MessengerType};
use crate::migration::MIGRATION_FILE_NAME;
use crate::service::Payload;
use crate::service_context::ServiceContext;
use crate::storage::testing::InMemoryStorageFactory;
use crate::tests::fakes::service_registry::ServiceRegistry;
use crate::tests::message_utils::verify_payload;
use crate::tests::scaffold::workload::channel;
use crate::{service, Environment, EnvironmentBuilder};
use ::fidl::endpoints::create_proxy_and_stream;
use assert_matches::assert_matches;
use fidl_fuchsia_stash::StoreMarker;
use fuchsia_async as fasync;
use fuchsia_inspect::component;
use futures::future::LocalBoxFuture;
use futures::{FutureExt, StreamExt};
use std::rc::Rc;

const ENV_NAME: &str = "settings_service_environment_test";
const TEST_PAYLOAD: &str = "test_payload";
const TEST_REPLY: &str = "test_reply";

// A test agent to send an event to the message hub. Required so that we can test that
// a message sent on the message hub returned from environment creation is received by
// other components attached to the message hub.
struct TestAgent {
    delegate: service::message::Delegate,
}

impl TestAgent {
    async fn create(mut context: Context) {
        let mut agent = TestAgent { delegate: context.delegate.clone() };

        fasync::Task::local(async move {
            let _ = &context;
            while let Ok((AgentPayload::Invocation(invocation), client)) =
                context.receptor.next_of::<AgentPayload>().await
            {
                let _ = client.reply(AgentPayload::Complete(agent.handle(invocation).await).into());
            }
        })
        .detach();
    }

    async fn handle(&mut self, invocation: Invocation) -> InvocationResult {
        match invocation.lifespan {
            Lifespan::Initialization => Err(AgentError::UnhandledLifespan),
            Lifespan::Service => self.handle_service_lifespan(invocation.service_context).await,
        }
    }

    async fn handle_service_lifespan(
        &mut self,
        _service_context: Rc<ServiceContext>,
    ) -> InvocationResult {
        let (_, mut receptor) = self.delegate.create_sink().await.expect("Failed to create broker");

        fasync::Task::local(async move {
            verify_payload(
                Payload::Event(EventPayload::Event(Event::Custom(TEST_PAYLOAD))),
                &mut receptor,
                Some(Box::new(|client| -> LocalBoxFuture<'_, ()> {
                    Box::pin(async move {
                        let _ = client
                            .reply(Payload::Event(EventPayload::Event(Event::Custom(TEST_REPLY))));
                    })
                })),
            )
            .await;
        })
        .detach();
        Ok(())
    }
}

// Ensure that the messenger factory returned from environment creation is able
// to send events to the test agent.
#[fuchsia::test(allow_stalls = false)]
async fn test_message_hub() {
    let service_registry = ServiceRegistry::create();

    let Environment { connector: _, delegate, .. } =
        EnvironmentBuilder::new(Rc::new(InMemoryStorageFactory::new()))
            .service(ServiceRegistry::serve(service_registry))
            .agents(vec![crate::create_agent!(test_agent, TestAgent::create)])
            .spawn_nested(ENV_NAME)
            .await
            .unwrap();

    // Send message for TestAgent to receive.
    let (messenger, _) =
        delegate.create(MessengerType::Unbound).await.expect("should be able to create messenger");

    let mut client_receptor = messenger.message(
        Payload::Event(EventPayload::Event(Event::Custom(TEST_PAYLOAD))),
        Audience::EventSink,
    );

    // Wait for reply from TestAgent.
    verify_payload(
        Payload::Event(EventPayload::Event(Event::Custom(TEST_REPLY))),
        &mut client_receptor,
        None,
    )
    .await;
}

#[fuchsia::test(allow_stalls = false)]
async fn test_dependency_generation() {
    let entity = Entity::Handler(SettingType::Unknown);

    let registrant = registration::Registrant::new(
        "Registrar::Test".to_string(),
        registration::Registrar::Test(Box::new(move || {})),
        [Dependency::Entity(entity)].into(),
    );

    let Environment { entities, .. } =
        EnvironmentBuilder::new(Rc::new(InMemoryStorageFactory::new()))
            .registrants(vec![registrant])
            .spawn_nested(ENV_NAME)
            .await
            .expect("environment should be built");

    assert!(entities.contains(&entity));
}

#[fuchsia::test(allow_stalls = false)]
async fn test_job_sourcing() {
    // Create channel to send the current job state.
    let (job_state_tx, mut job_state_rx) = futures::channel::mpsc::unbounded::<channel::State>();

    // Create a new job stream with an Job that will signal when it is executed.
    let job_stream = async move {
        Ok(Job::new(job::work::Load::Independent(Box::new(channel::Workload::new(job_state_tx)))))
            as Result<Job, Error>
    }
    .into_stream();

    // Build a registrant with the stream.
    let registrant = registration::Registrant::new(
        "Registrar::TestWithSeeder".to_string(),
        registration::Registrar::TestWithSeeder(Box::new(move |seeder| {
            seeder.seed(job_stream);
        })),
        [].into(),
    );

    // Build environment with the registrant.
    let _ = EnvironmentBuilder::new(Rc::new(InMemoryStorageFactory::new()))
        .registrants(vec![registrant])
        .spawn_nested(ENV_NAME)
        .await
        .expect("environment should be built");

    // Ensure job is executed.
    assert_matches!(job_state_rx.next().await, Some(channel::State::Execute));
}

#[fuchsia::test]
async fn migration_error_does_not_cause_early_exit() {
    const UNKNOWN_ID: u64 = u64::MAX;
    let fs = tempfile::tempdir().expect("failed to create tempdir");
    std::fs::write(fs.path().join(MIGRATION_FILE_NAME), UNKNOWN_ID.to_string())
        .expect("failed to write migration file");
    let directory = fuchsia_fs::directory::open_in_namespace(
        fs.path().to_str().expect("tempdir path is not valid UTF-8"),
        fuchsia_fs::PERM_READABLE | fuchsia_fs::PERM_WRITABLE,
    )
    .expect("failed to open connection to tempdir");
    let (store_proxy, mut request_stream) =
        create_proxy_and_stream::<StoreMarker>().expect("can create");
    fasync::Task::local(async move {
        while let Some(request) = request_stream.next().await {
            match request.unwrap() {
                fidl_fuchsia_stash::StoreRequest::Identify { .. } => {}
                fidl_fuchsia_stash::StoreRequest::CreateAccessor { accessor_request, .. } => {
                    let mut stream = accessor_request.into_stream().unwrap();
                    fasync::Task::local(async move {
                        if let Some(r) = stream.next().await {
                            panic!("unexpected call to store before migration id checked: {r:?}");
                        }
                    })
                    .detach();
                }
            }
        }
    })
    .detach();

    let config_logger =
        Rc::new(std::sync::Mutex::new(InspectConfigLogger::new(component::inspector().root())));

    let _ = EnvironmentBuilder::new(Rc::new(InMemoryStorageFactory::new()))
        .fidl_interfaces(&[fidl::Interface::Light])
        .store_proxy(store_proxy)
        .storage_dir(directory)
        .light_configuration(build_light_default_settings(config_logger))
        .spawn_nested(ENV_NAME)
        .await
        .expect("environment should be built");
}
