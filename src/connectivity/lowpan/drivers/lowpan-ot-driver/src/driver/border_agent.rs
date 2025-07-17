// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use super::*;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_net_mdns::*;
use futures::channel::mpsc;
use futures::future::Fuse;
use futures::select;
use openthread::ot::BorderAgentEphemeralKeyState;
use rand;
use std::pin::pin;

const BORDER_AGENT_SERVICE_TYPE: &str = "_meshcop._udp.";
const BORDER_AGENT_EPSKC_SERVICE_TYPE: &str = "_meshcop-e._udp.";

// Port 9 is the old-school discard port.
const BORDER_AGENT_SERVICE_PLACEHOLDER_PORT: u16 = 9;

// Number of times to attempt to publish services.
const MAX_PUBLISH_SERVICE_ATTEMPTS: usize = 2;

// These flags are ultimately defined by table 8-5 of the Thread v1.1.1 specification.
// Additional flags originate from the source code found [here][1].
//
// [1]: https://github.com/openthread/ot-br-posix/blob/36db8891576a6ed571ad319afca734c5288c4cd9/src/border_agent/border_agent.cpp#L86
bitflags::bitflags! {
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BorderAgentState : u32 {
    const CONNECTION_MODE_PSKC = 1;
    const CONNECTION_MODE_PSKD = 2;
    const CONNECTION_MODE_VENDOR = 3;
    const CONNECTION_MODE_X509 = 4;
    const THREAD_IF_STATUS_INITIALIZED = (1<<3);
    const THREAD_IF_STATUS_ACTIVE = (2<<3);
    const HIGH_AVAILABILITY = (1<<5);
    const BBR_IS_ACTIVE = (1<<7);
    const BBR_IS_PRIMARY = (1<<8);
    const EPSKC_SUPPORTED = (1<<11);
}
}

fn calc_meshcop_service_txt<OT>(
    ot_instance: &OT,
    vendor: &str,
    product: &str,
) -> Vec<(String, Vec<u8>)>
where
    OT: ot::InstanceInterface,
{
    let mut txt: Vec<(String, Vec<u8>)> = Vec::new();

    let mut border_agent_state = BorderAgentState::HIGH_AVAILABILITY;

    if ot_instance.border_agent_is_active() == true {
        border_agent_state |= BorderAgentState::CONNECTION_MODE_PSKC;
    }

    if ot_instance.is_commissioned() {
        match ot_instance.get_device_role() {
            ot::DeviceRole::Disabled => {
                border_agent_state |= BorderAgentState::THREAD_IF_STATUS_INITIALIZED
            }
            _ => border_agent_state |= BorderAgentState::THREAD_IF_STATUS_ACTIVE,
        }
    }

    // `rv` - Version of TXT record format.
    txt.push(("rv".to_string(), b"1".to_vec()));

    // `tv` - Version of Thread specification in use.
    txt.push(("tv".to_string(), ot::get_thread_version_str().as_bytes().to_vec()));

    // `sb` - State bitmap
    txt.push(("sb".to_string(), border_agent_state.bits().to_be_bytes().to_vec()));

    // `nn` - Network Name
    if ot_instance.is_commissioned() {
        match ot_instance.get_network_name().try_as_str() {
            Ok(nn) => txt.push(("nn".to_string(), nn.as_bytes().to_vec())),
            Err(err) => {
                warn!("Can't render network name: {:?}", err);
            }
        }

        // `xp` - Extended PAN-ID
        txt.push(("xp".to_string(), ot_instance.get_extended_pan_id().as_slice().to_vec()));
    }

    // `vn` - Vendor Name
    txt.push(("vn".to_string(), vendor.as_bytes().to_vec()));

    // `mn` - Model Name
    txt.push(("mn".to_string(), product.as_bytes().to_vec()));

    // `xa` - Extended Address
    txt.push(("xa".to_string(), ot_instance.get_extended_address().as_slice().to_vec()));

    if ot_instance.get_device_role().is_active() {
        let mut dataset = Default::default();

        match ot_instance.dataset_get_active(&mut dataset) {
            Ok(()) => {
                if let Some(at) = dataset.get_active_timestamp() {
                    // `at` - Active Operational Dataset Timestamp
                    txt.push(("at".to_string(), at.to_be_bytes().to_vec()));
                }
            }

            Err(err) => {
                warn!(tag = "meshcop"; "Failed to get active dataset: {:?}", err);
            }
        }

        // `pt` - Partition ID
        txt.push(("pt".to_string(), ot_instance.get_partition_id().to_be_bytes().to_vec()));
    }

    txt
}

enum PublishServiceFailure {
    PublishInstanceRequestFailure,
    PublishInstanceFailure,
    ResponderFailure,
}

async fn publish_service(
    tag: &str,
    service_type: &str,
    service_instance: &str,
    txt: Option<Vec<(String, Vec<u8>)>>,
    port: u16,
    publisher: ServiceInstancePublisherProxy,
) -> Result<(), PublishServiceFailure> {
    let (client, server) = create_endpoints::<ServiceInstancePublicationResponder_Marker>();
    let publish_init_future = publisher
        .publish_service_instance(
            service_type,
            service_instance,
            &ServiceInstancePublicationOptions::default(),
            client,
        )
        .map(|x| -> Result<(), PublishServiceFailure> {
            match x {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => {
                    error!(tag = tag; "publish_init_future failed: {:?}", err);
                    Err(PublishServiceFailure::PublishInstanceFailure)
                }
                Err(zx_err) => {
                    error!(tag = tag; "publish_init_future failed: {:?}", zx_err);
                    Err(PublishServiceFailure::PublishInstanceRequestFailure)
                }
            }
        });

    // Prepare our static response for all queries.
    let publication = ServiceInstancePublication {
        port: Some(port),
        text: txt.map(|txt| {
            txt.iter()
                .map(|(key, value)| {
                    let mut x = key.as_bytes().to_vec();
                    x.push(b'=');
                    x.extend(value.as_slice());
                    x
                })
                .collect::<Vec<_>>()
        }),
        ..Default::default()
    };

    let publish_responder_future = server.into_stream().map_err(Into::into).try_for_each(
        move |ServiceInstancePublicationResponder_Request::OnPublication {
                  publication_cause,
                  subtype,
                  source_addresses,
                  responder,
              }| {
            debug!(
                tag = tag;
                "publication_cause: {publication_cause:?}"
            );
            debug!(tag = tag; "publication_cause: {subtype:?}");
            debug!(
                tag = tag;
                "source_addresses: {source_addresses:?}"
            );
            debug!(
                tag = tag;
                "publication: {:?}", &publication
            );

            // Due to https://fxbug.dev/42182233, the publication responder channel will close
            // if the publisher that created it is closed.
            // TODO(https://fxbug.dev/42182233): Remove this line once https://fxbug.dev/42182233 is fixed.
            let _publisher = publisher.clone();

            let result = if subtype.is_some() {
                debug!(
                    tag = tag;
                    "Subtype specified, skipping advertisement."
                );
                Err(OnPublicationError::DoNotRespond)
            } else {
                Ok(&publication)
            };

            futures::future::ready(
                responder.send(result).context("Unable to call publication responder"),
            )
        },
    );

    futures::try_join!(
        publish_init_future,
        publish_responder_future.map_err(|err| {
            error!(tag = tag; "publish_responder_future failed: {:?}", err);
            PublishServiceFailure::ResponderFailure
        }),
    )?;

    Ok(())
}

async fn publish_border_agent_service(
    service_instance: String,
    txt: Vec<(String, Vec<u8>)>,
    port: u16,
    publisher: ServiceInstancePublisherProxy,
) -> Result<(), anyhow::Error> {
    let tag = "meshcop";

    for i in 0..MAX_PUBLISH_SERVICE_ATTEMPTS {
        let service_name = match i {
            0 => service_instance.clone(),
            _ => get_alternate_service_instance_name(service_instance.as_str()),
        };

        match publish_service(
            tag,
            BORDER_AGENT_SERVICE_TYPE,
            service_name.as_str(),
            Some(txt.clone()),
            port,
            publisher.clone(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(PublishServiceFailure::PublishInstanceRequestFailure) => {
                return Err(anyhow::format_err!("Failed to publish border agent service."))
            }
            Err(PublishServiceFailure::PublishInstanceFailure)
            | Err(PublishServiceFailure::ResponderFailure) => {
                warn!(tag; "Publish attempt failed.",);
            }
        }
    }

    Err(anyhow::format_err!("Exhausted service publication retry attempts."))
}

pub(crate) enum PublishServiceRequest {
    Stop,
    Start { port: u16, service_instance: String },
}

async fn publish_epskc_service(
    service_instance: String,
    port: u16,
    publisher: ServiceInstancePublisherProxy,
) -> Result<(), anyhow::Error> {
    let tag = "meshcop-e";

    for i in 0..MAX_PUBLISH_SERVICE_ATTEMPTS {
        let service_name = match i {
            0 => service_instance.clone(),
            _ => get_alternate_service_instance_name(service_instance.as_str()),
        };

        match publish_service(
            tag,
            BORDER_AGENT_EPSKC_SERVICE_TYPE,
            service_name.as_str(),
            None,
            port,
            publisher.clone(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(PublishServiceFailure::PublishInstanceRequestFailure) => {
                return Err(anyhow::format_err!("Failed to publish ePSKc service."))
            }
            Err(PublishServiceFailure::PublishInstanceFailure)
            | Err(PublishServiceFailure::ResponderFailure) => {
                warn!(tag; "Publish attempt failed.",);
            }
        }
    }

    Err(anyhow::format_err!("Exhausted service publication retry attempts."))
}

pub(crate) async fn manage_epskc_service_publisher(
    mut receiver: mpsc::Receiver<PublishServiceRequest>,
    publisher: ServiceInstancePublisherProxy,
) -> Result<(), anyhow::Error> {
    let publication_fut = Fuse::terminated();
    let mut publication_fut = pin!(publication_fut);

    loop {
        select! {
            result = publication_fut => {
                info!("ePSKc publication completed: {result:?}");
            }
            state = receiver.select_next_some() => {
                match state {
                    PublishServiceRequest::Start {port, service_instance} => {
                        // Begin publishing the new service description.  Any existing publication
                        // action will be dropped.
                        let cloned_publisher = publisher.clone();
                        publication_fut.set(async move {
                            publish_epskc_service(
                            service_instance,
                            port,
                            cloned_publisher,
                        ).await
                        }.fuse());
                    }
                    PublishServiceRequest::Stop => {
                        // Set the publication future to a terminated future to stop any ongoing
                        // publication.
                        publication_fut.set(Fuse::terminated());
                    }
                }
            }
        }
    }
}

// Functional equivalent to ot-br-posix's BorderAgent::GetServiceInstanceNameWithExtAddr.
fn get_service_instance_name_with_ext_addr(vendor: &str, product: &str, ext_addr: &[u8]) -> String {
    format!("{} {} #{:0X}{:0X}", vendor, product, ext_addr[6], ext_addr[7])
}

fn get_alternate_service_instance_name(base_instance_name: &str) -> String {
    let rand_u16 = rand::random::<u16>();
    format!("{} ({})", base_instance_name, rand_u16)
}

impl<OT: ot::InstanceInterface, NI, BI> OtDriver<OT, NI, BI> {
    pub async fn update_border_agent_service(&self) {
        let vendor = self.product_metadata.vendor();
        let product = self.product_metadata.product();

        // Add the last two bytes (in hex) of the extended address to the device name
        // to make the name more stable.
        let service_instance_name = {
            let driver_state = self.driver_state.lock();
            let ot_instance = &driver_state.ot_instance;
            format!(
                "{} ({})",
                product,
                hex::encode(&ot_instance.get_extended_address().as_slice()[6..])
            )
        };

        let (txt, port) = {
            let mut txt = self.border_agent_vendor_txt_entries.lock().await.clone();

            let driver_state = self.driver_state.lock();
            let ot_instance = &driver_state.ot_instance;
            txt.extend(calc_meshcop_service_txt(ot_instance, &vendor, &product));
            let port = if ot_instance.border_agent_is_active() == true {
                ot_instance.border_agent_get_udp_port()
            } else {
                // The following comment is from the original ot-br-posix implementation:
                // ---
                // When thread interface is not active, the border agent is not started,
                // thus it's not listening to any port and not handling requests. In such
                // situation, we use a placeholder port number for publishing the MeshCoP
                // service to advertise the status of the border router. One can learn
                // the thread interface status from `sb` entry so it doesn't have to send
                // requests to the placeholder port when border agent is not running.
                BORDER_AGENT_SERVICE_PLACEHOLDER_PORT
            };
            (txt, port)
        };

        let border_agent_current_txt_entries = self.border_agent_current_txt_entries.clone();
        let mut last_txt_entries = border_agent_current_txt_entries.lock().await;

        if txt == *last_txt_entries {
            debug!(tag = "meshcop"; "update_border_agent_service: No changes.");
        } else {
            debug!(
                tag = "meshcop";
                "update_border_agent_service: Updating meshcop dns-sd: port={} txt=[PII]({:?})",
                port,
                txt
            );

            *last_txt_entries = txt.clone();

            let old_service = self.border_agent_service.lock().take();
            if let Some(task) = old_service {
                if let Some(Err(err)) = task.abort().await {
                    warn!(
                        tag = "meshcop";
                        "update_border_agent_service: Previous publication task ended with an \
                         error: {err:?}"
                    );
                }
                info!(tag = "meshcop"; "update_border_agent_service: pervious task terminated");
            }

            *self.border_agent_service.lock() =
                Some(fasync::Task::spawn(publish_border_agent_service(
                    service_instance_name,
                    txt,
                    port,
                    self.publisher.clone(),
                )));
        }
    }

    pub fn handle_epskc_state_changed(&self) {
        // Get all of the state information from OT that requires locking on OtDriver fields.
        // The current ePSKc state determines what action will be taken if any.  If the service has
        // been started, the extended address is used in deriving the service instance name and the
        // port is published to listeners.
        let (state, port, ext_addr) = {
            let driver_state = self.driver_state.lock();
            let ot_instance = &driver_state.ot_instance;
            let state = ot_instance.border_agent_ephemeral_key_get_state();
            let port = ot_instance.border_agent_ephemeral_key_get_udp_port();
            let ext_addr = ot_instance.get_extended_address().as_slice().to_vec();

            (state, port, ext_addr)
        };

        let mut epskc_publisher = self.epskc_publisher.lock();

        info!(
            tag = "meshcop-e"; "handle_epskc_state_changed: handling state change {:?}", state
        );
        match state {
            BorderAgentEphemeralKeyState::Started => {
                // Started: "Ephemeral key is set. Listening to accept secure connections."
                //
                // When a new ephemeral key is set, stop any existing publication and publish the
                // latest information.

                // Derive the service name.
                let vendor = self.product_metadata.vendor();
                let product = self.product_metadata.product();
                let service_instance =
                    get_service_instance_name_with_ext_addr(&vendor, &product, ext_addr.as_slice());

                if let Err(e) = epskc_publisher
                    .try_send(PublishServiceRequest::Start { port, service_instance })
                {
                    warn!("Could not post Start event: {e:}")
                };
            }
            BorderAgentEphemeralKeyState::Disabled | BorderAgentEphemeralKeyState::Stopped => {
                // Disabled: "Ephemeral Key Manager is disabled."
                // Stopped: "Enabled, but no ephemeral key is in use (not set or started)."
                //
                // If the ePSKc feature is disabled or no ephemeral key is available for use, ensure
                // that the service is no longer published.
                if let Err(e) = epskc_publisher.try_send(PublishServiceRequest::Stop) {
                    warn!("Could not post Stop event: {e:}")
                }
            }
            BorderAgentEphemeralKeyState::Connected | BorderAgentEphemeralKeyState::Accepted => {
                // Here is the documentation associated with these states:
                //
                // Connected: "Session is established with an external commissioner candidate."
                // Accepted: "Session is established and candidate is accepted as full commissioner."
                //
                // These state transitions are internal to OpenThread.  No action is required from
                // the platform as the ePSKc service must already be actively published in order for
                // these states to occur.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy, Proxy};
    use fuchsia_async::TestExecutor;
    use lazy_static::lazy_static;
    use std::task::Poll;
    use {fidl_fuchsia_net_mdns as fidl_mdns, regex};

    const TEST_PORT: u16 = 1234;
    const TEST_SERVICE: &str = "test test test";

    lazy_static! {
        static ref TEST_TEXT: Vec<(String, Vec<u8>)> = vec![
            (String::from("abcd"), vec![1, 2, 3, 4]),
            (String::from("wxyz"), vec![5, 6, 7, 8]),
        ];
    }

    struct TestValues {
        publisher: ServiceInstancePublisherProxy,
        publisher_stream: ServiceInstancePublisherRequestStream,
        sender: mpsc::Sender<PublishServiceRequest>,
        receiver: mpsc::Receiver<PublishServiceRequest>,
    }

    fn test_setup() -> TestValues {
        let (publisher, publisher_reqs) =
            create_proxy::<fidl_mdns::ServiceInstancePublisherMarker>();
        let publisher_stream = publisher_reqs.into_stream();

        let (sender, receiver) = mpsc::channel(100);

        return TestValues { publisher, publisher_stream, sender, receiver };
    }

    #[fuchsia::test]
    fn test_publish_border_agent_service_publication_failure() {
        // Use an executor with fake time to prevent the timers related to the fake spinel instance
        // from failing the test.
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_border_agent_service(
            String::from(TEST_SERVICE),
            TEST_TEXT.to_vec(),
            TEST_PORT,
            test_vals.publisher,
        );
        let mut fut = pin!(fut);

        for i in 0..MAX_PUBLISH_SERVICE_ATTEMPTS {
            // Progress the future and expect it to stall while attempting to interact with MDNS.
            assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

            // There should now be a request from the publisher.
            assert_matches!(
                exec.run_until_stalled(&mut test_vals.publisher_stream.next()),
                Poll::Ready(Some(Ok(fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service, instance, options, publication_responder: _, responder,
                }))) => {
                        match i {

                            0 => assert_eq!(instance, TEST_SERVICE),
                            _ => {
                                let re = regex::Regex::new(
                                    (TEST_SERVICE.to_owned() + " \\([0-9]+\\)").as_str()
                                ).unwrap();
                                assert!(re.is_match(&instance))
                            }
                        }
                    assert_eq!(service, BORDER_AGENT_SERVICE_TYPE);
                    assert_eq!(options, ServiceInstancePublicationOptions::default());
                    responder
                        .send(Err(fidl_mdns::PublishServiceInstanceError::AlreadyPublishedLocally))
                        .expect("Failed to send publish response");
                }
            );
        }

        // The future should complete with an error.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_epskc_service_publication_failure() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_epskc_service(String::from(TEST_SERVICE), TEST_PORT, test_vals.publisher);
        let mut fut = pin!(fut);

        for i in 0..MAX_PUBLISH_SERVICE_ATTEMPTS {
            // Progress the future and expect it to stall while attempting to interact with MDNS.
            assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

            // There should now be a request from the publisher.
            assert_matches!(
                exec.run_until_stalled(&mut test_vals.publisher_stream.next()),
                Poll::Ready(Some(Ok(fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service, instance, options, publication_responder: _, responder,
                }))) => {
                        match i {
                            0 => assert_eq!(instance, TEST_SERVICE),
                            _ => {
                                let re = regex::Regex::new(
                                    (TEST_SERVICE.to_owned() + " \\([0-9]+\\)").as_str()
                                ).unwrap();
                                assert!(re.is_match(&instance))
                            }
                        }
                    assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                    assert_eq!(options, ServiceInstancePublicationOptions::default());
                    responder
                        .send(Err(fidl_mdns::PublishServiceInstanceError::AlreadyPublishedLocally))
                        .expect("Failed to send publish response");
                }
            );
        }

        // The future should complete with an error.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_publish_border_agent_service_exits_when_publication_service_drops() {
        let mut exec = TestExecutor::new();
        let test_vals = test_setup();

        // Drop the publisher stream.
        drop(test_vals.publisher_stream);

        let fut = publish_border_agent_service(
            String::from(TEST_SERVICE),
            TEST_TEXT.to_vec(),
            TEST_PORT,
            test_vals.publisher,
        );
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_publish_epskc_service_exits_when_publication_service_drops() {
        let mut exec = TestExecutor::new();
        let test_vals = test_setup();

        // Drop the publisher stream.
        drop(test_vals.publisher_stream);

        let fut = publish_epskc_service(String::from(TEST_SERVICE), TEST_PORT, test_vals.publisher);
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_publish_border_agent_service_publication_success() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_border_agent_service(
            String::from(TEST_SERVICE),
            TEST_TEXT.to_vec(),
            TEST_PORT,
            test_vals.publisher,
        );
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // There should now be a request from the publisher.
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };
        let responder = responder.into_proxy();

        // The future should still be running.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Publish
        let publication_fut =
            responder.on_publication(ServiceInstancePublicationCause::Announcement, None, &[]);
        let mut publication_fut = pin!(publication_fut);
        assert_matches!(exec.run_until_stalled(&mut publication_fut), Poll::Pending);

        // Run the border agent future and observe:
        // 1. The border agent future is still running.
        // 2. The publication future completes.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_matches!(
            exec.run_until_stalled(&mut publication_fut),
            Poll::Ready(Ok(Ok(ServiceInstancePublication { port, text, .. }))) => {
                assert_eq!(port, Some(TEST_PORT));
                assert_eq!(text, Some(vec![
                    // asdf=1234
                    vec![97, 98, 99, 100, 61, 1, 2, 3, 4],
                    // wxyz=5678
                    vec![119, 120, 121, 122, 61, 5, 6, 7, 8],
                ]))
            }
        );

        // Verify that the publisher proxy is still held.
        assert_matches!(
            exec.run_until_stalled(&mut test_vals.publisher_stream.next()),
            Poll::Pending
        );
    }

    #[fuchsia::test]
    fn test_publish_espkc_service_publication_success() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_epskc_service(String::from(TEST_SERVICE), TEST_PORT, test_vals.publisher);
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // There should now be a request from the publisher.
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };
        let responder = responder.into_proxy();

        // The future should still be running.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Publish
        let publication_fut =
            responder.on_publication(ServiceInstancePublicationCause::Announcement, None, &[]);
        let mut publication_fut = pin!(publication_fut);
        assert_matches!(exec.run_until_stalled(&mut publication_fut), Poll::Pending);

        // Run the border agent future and observe:
        // 1. The border agent future is still running.
        // 2. The publication future completes.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_matches!(
            exec.run_until_stalled(&mut publication_fut),
            Poll::Ready(Ok(Ok(ServiceInstancePublication { port, .. }))) => {
                assert_eq!(port, Some(TEST_PORT));
            }
        );

        // Verify that the publisher proxy is still held.
        assert_matches!(
            exec.run_until_stalled(&mut test_vals.publisher_stream.next()),
            Poll::Pending
        );
    }

    #[fuchsia::test]
    fn test_publish_border_agent_service_exits_when_publisher_drops_responder() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_border_agent_service(
            String::from(TEST_SERVICE),
            TEST_TEXT.to_vec(),
            TEST_PORT,
            test_vals.publisher,
        );
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // There should now be a request from the publisher.
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };
        drop(responder);

        // The future should complete.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
    }

    #[fuchsia::test]
    fn test_publish_epskc_service_exits_when_publisher_drops_responder() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = publish_epskc_service(String::from(TEST_SERVICE), TEST_PORT, test_vals.publisher);
        let mut fut = pin!(fut);

        // Progress the future and expect it to stall while attempting to interact with MDNS.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // There should now be a request from the publisher.
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };
        drop(responder);

        // The future should complete.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
    }

    #[fuchsia::test]
    fn test_manage_epskc_service_publisher_publishes_service() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = manage_epskc_service_publisher(test_vals.receiver, test_vals.publisher.clone());
        let mut fut = pin!(fut);

        // Initially nothing happens.  The future should simply block waiting for requests.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Make a request to start.
        test_vals
            .sender
            .try_send(PublishServiceRequest::Start {
                port: TEST_PORT,
                service_instance: String::from(TEST_SERVICE),
            })
            .expect("failed to send start request");

        // Progress the future and observe that it has made a publication request.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };

        // Progress the future again and observe that it is still running.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let responder = responder.into_proxy();
        assert!(!responder.is_closed());
    }

    #[fuchsia::test]
    fn test_manage_epskc_service_publisher_stops_publishing() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = manage_epskc_service_publisher(test_vals.receiver, test_vals.publisher.clone());
        let mut fut = pin!(fut);

        // Initially nothing happens.  The future should simply block waiting for requests.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Make a request to start.
        test_vals
            .sender
            .try_send(PublishServiceRequest::Start {
                port: TEST_PORT,
                service_instance: String::from(TEST_SERVICE),
            })
            .expect("failed to send start request");

        // Progress the future and observe that it has made a publication request.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };

        // Progress the future again and observe that it is still running.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let responder = responder.into_proxy();
        assert!(!responder.is_closed());

        // Now request that publishing stop.
        test_vals
            .sender
            .try_send(PublishServiceRequest::Stop)
            .expect("failed to make stop request");

        // Progress the future and observe that no new publication requests are made and the
        // existing publication channel has been dropped.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_matches!(
            exec.run_until_stalled(&mut test_vals.publisher_stream.next()),
            Poll::Pending
        );
        assert!(responder.is_closed())
    }

    #[fuchsia::test]
    fn test_manage_epskc_service_publisher_overlapping_start_requests() {
        let mut exec = TestExecutor::new();
        let mut test_vals = test_setup();

        let fut = manage_epskc_service_publisher(test_vals.receiver, test_vals.publisher.clone());
        let mut fut = pin!(fut);

        // Initially nothing happens.  The future should simply block waiting for requests.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Make a request to start.
        test_vals
            .sender
            .try_send(PublishServiceRequest::Start {
                port: TEST_PORT,
                service_instance: String::from(TEST_SERVICE),
            })
            .expect("failed to send start request");

        // Progress the future and observe that it has made a publication request.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next()) {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };

        // Progress the future again and observe that it is still running.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let responder = responder.into_proxy();
        assert!(!responder.is_closed());

        // Make another start request.
        test_vals
            .sender
            .try_send(PublishServiceRequest::Start {
                port: TEST_PORT,
                service_instance: String::from(TEST_SERVICE),
            })
            .expect("failed to send overlapping start request");

        // Progress the future and observe that another publish request has been made and the
        // original responder channel is closed.
        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let _second_responder = match exec.run_until_stalled(&mut test_vals.publisher_stream.next())
        {
            Poll::Ready(Some(Ok(
                fidl_mdns::ServiceInstancePublisherRequest::PublishServiceInstance {
                    service,
                    instance,
                    options,
                    publication_responder,
                    responder,
                },
            ))) => {
                assert_eq!(service, BORDER_AGENT_EPSKC_SERVICE_TYPE);
                assert_eq!(instance, TEST_SERVICE);
                assert_eq!(options, ServiceInstancePublicationOptions::default());
                responder.send(Ok(())).expect("Failed to send publish response");

                publication_responder
            }
            other => panic!("Unexpected variant: {:?}", other),
        };
        assert!(responder.is_closed());
    }
}
