// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::action_fuse::ActionFuse;
use crate::message::base::{Audience, Filter, MessageEvent, MessengerType, Status};
use crate::message::receptor::Receptor;
use crate::tests::message_utils::verify_payload;
use fuchsia_zircon::DurationNum;
use futures::future::BoxFuture;
use futures::lock::Mutex;
use futures::StreamExt;
use std::sync::Arc;
use std::task::Poll;

type TestPayload = crate::service::test::Payload;
pub(crate) mod test_message {
    use super::TestPayload;
    pub static FOO: crate::Payload = crate::Payload::Test(TestPayload::Integer(0));
    pub static BAR: crate::Payload = crate::Payload::Test(TestPayload::Integer(1));
    pub static BAZ: crate::Payload = crate::Payload::Test(TestPayload::Integer(2));
    pub static QUX: crate::Payload = crate::Payload::Test(TestPayload::Integer(3));
    pub static THUD: crate::Payload = crate::Payload::Test(TestPayload::Integer(4));
}

/// Ensures the delivery result matches expected value.
async fn verify_result(expected: Status, receptor: &mut Receptor) {
    while let Some(message_event) = receptor.next().await {
        if let MessageEvent::Status(status) = message_event {
            if status == expected {
                return;
            }
        }
    }

    panic!("Didn't receive result expected");
}

static ORIGINAL: &crate::Payload = &test_message::FOO;
static MODIFIED: &crate::Payload = &test_message::QUX;
static MODIFIED_2: &crate::Payload = &test_message::THUD;
static BROADCAST: &crate::Payload = &test_message::BAZ;
static REPLY: &crate::Payload = &test_message::BAR;

mod test {
    pub(crate) type MessageHub = crate::message::message_hub::MessageHub;
}

fn no_filter() -> Filter {
    Arc::new(|&_| true)
}

// Tests message client creation results in unique ids.
#[fuchsia::test(allow_stalls = false)]
async fn test_message_client_equality() {
    let delegate = test::MessageHub::create();
    let (messenger, _) = delegate.create(MessengerType::Unbound).await.unwrap();
    let (_, mut receptor) = delegate.create(MessengerType::Unbound).await.unwrap();

    let _ = messenger.message(ORIGINAL.clone(), Audience::Broadcast);
    let (_, client_1) = receptor.next_payload().await.unwrap();

    let _ = messenger.message(ORIGINAL.clone(), Audience::Broadcast);
    let (_, client_2) = receptor.next_payload().await.unwrap();

    assert!(client_1 != client_2);

    assert_eq!(client_1, client_1.clone());
}

// Tests messenger creation and address space collision.
#[fuchsia::test(allow_stalls = false)]
async fn test_messenger_creation() {
    let delegate = test::MessageHub::create();
    let address = crate::Address::Test(1);

    let messenger_1_result = delegate.create(MessengerType::Addressable(address)).await;
    assert!(messenger_1_result.is_ok());

    assert!(delegate.create(MessengerType::Addressable(address)).await.is_err());
}

// Tests whether the client is reported as present after being created.
#[fuchsia::test(allow_stalls = false)]
async fn test_messenger_presence() {
    let delegate = test::MessageHub::create();

    // Create unbound messenger
    let (_, receptor) =
        delegate.create(MessengerType::Unbound).await.expect("messenger should be created");

    // Check for messenger's presence
    assert!(delegate.contains(receptor.get_signature()).await.expect("check should complete"));

    // Check for an address that shouldn't exist
    #[allow(clippy::bool_assert_comparison)]
    {
        assert_eq!(
            delegate
                .contains(crate::message::base::Signature::Address(crate::Address::Test(1)))
                .await
                .expect("check should complete"),
            false
        );
    }
}

// Tests messenger creation and address space collision.
#[fuchsia::test(allow_stalls = false)]
async fn test_messenger_deletion() {
    let delegate = test::MessageHub::create();
    let address = crate::Address::Test(1);

    {
        let (_, _) = delegate.create(MessengerType::Addressable(address)).await.unwrap();

        // By the time this subsequent create happens, the previous messenger and
        // receptor belonging to this address should have gone out of scope and
        // freed up the address space.
        assert!(delegate.create(MessengerType::Addressable(address)).await.is_ok());
    }

    {
        // Holding onto the MessengerClient should prevent deletion.
        let (_messenger_client, _) =
            delegate.create(MessengerType::Addressable(address)).await.unwrap();
        assert!(delegate.create(MessengerType::Addressable(address)).await.is_err());
    }

    {
        // Holding onto the Receptor should prevent deletion.
        let (_, _receptor) = delegate.create(MessengerType::Addressable(address)).await.unwrap();
        assert!(delegate.create(MessengerType::Addressable(address)).await.is_err());
    }
}

#[fuchsia::test(allow_stalls = false)]
async fn test_messenger_deletion_with_fingerprint() {
    let delegate = test::MessageHub::create();
    let address = crate::Address::Test(1);
    let (_, mut receptor) =
        delegate.create(MessengerType::Addressable(address)).await.expect("should get receptor");
    delegate.delete(receptor.get_signature());
    assert!(receptor.next().await.is_none());
}

// Tests basic functionality of the MessageHub, ensuring messages and replies
// are properly delivered.
#[fuchsia::test(allow_stalls = false)]
async fn test_end_to_end_messaging() {
    let delegate = test::MessageHub::create();

    let (messenger_client_1, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    let (_, mut receptor_2) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(2))).await.unwrap();

    let mut reply_receptor =
        messenger_client_1.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(2)));

    verify_payload(
        ORIGINAL.clone(),
        &mut receptor_2,
        Some(Box::new(|client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    verify_payload(REPLY.clone(), &mut reply_receptor, None).await;
}

// Tests forwarding behavior, making sure a message is forwarded in the case
// the client does nothing with it.
#[fuchsia::test(allow_stalls = false)]
async fn test_implicit_forward() {
    let delegate = test::MessageHub::create();

    let (messenger_client_1, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    let (_, mut receiver_2) = delegate.create(MessengerType::Broker(no_filter())).await.unwrap();
    let (_, mut receiver_3) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(3))).await.unwrap();

    let mut reply_receptor =
        messenger_client_1.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(3)));

    // Ensure observer gets payload and then do nothing with message.
    verify_payload(ORIGINAL.clone(), &mut receiver_2, None).await;

    verify_payload(
        ORIGINAL.clone(),
        &mut receiver_3,
        Some(Box::new(|client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    // Ensure observer gets payload and then do nothing with message.
    verify_payload(REPLY.clone(), &mut receiver_2, None).await;

    verify_payload(REPLY.clone(), &mut reply_receptor, None).await;
}

// Exercises the observation functionality. Makes sure a broker who has
// indicated they would like to participate in a message path receives the
// reply.
#[fuchsia::test(allow_stalls = false)]
async fn test_observe_addressable() {
    let delegate = test::MessageHub::create();

    let (messenger_client_1, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    let (_, mut receptor_2) = delegate.create(MessengerType::Broker(no_filter())).await.unwrap();
    let (_, mut receptor_3) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(3))).await.unwrap();

    let mut reply_receptor =
        messenger_client_1.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(3)));

    let observe_receptor = Arc::new(Mutex::new(None));
    verify_payload(ORIGINAL.clone(), &mut receptor_2, {
        let observe_receptor = observe_receptor.clone();
        Some(Box::new(move |mut client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let mut receptor = observe_receptor.lock().await;
                *receptor = Some(client.spawn_observer());
            })
        }))
    })
    .await;

    verify_payload(
        ORIGINAL.clone(),
        &mut receptor_3,
        Some(Box::new(|client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    if let Some(mut receptor) = observe_receptor.lock().await.take() {
        verify_payload(REPLY.clone(), &mut receptor, None).await;
    } else {
        panic!("A receptor should have been assigned")
    }
    verify_payload(REPLY.clone(), &mut reply_receptor, None).await;
}

// Validates that timeout status is reached when there is no response
#[fuchsia::test]
fn test_timeout() {
    let mut executor = fuchsia_async::TestExecutor::new_with_fake_time();
    let timeout_ms = 1000;

    let fut = async move {
        let delegate = test::MessageHub::create();
        let (messenger_client_1, _) =
            delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
        let (_, mut receptor_2) =
            delegate.create(MessengerType::Addressable(crate::Address::Test(2))).await.unwrap();

        let mut reply_receptor = messenger_client_1.message_with_timeout(
            ORIGINAL.clone(),
            Audience::Address(crate::Address::Test(2)),
            Some(timeout_ms.millis()),
        );

        verify_payload(
            ORIGINAL.clone(),
            &mut receptor_2,
            Some(Box::new(|_| -> BoxFuture<'_, ()> {
                Box::pin(async move {
                    // Do not respond.
                })
            })),
        )
        .await;

        verify_result(Status::Timeout, &mut reply_receptor).await;
    };

    pin_utils::pin_mut!(fut);
    loop {
        let new_time = fuchsia_async::Time::from_nanos(
            executor.now().into_nanos()
                + fuchsia_zircon::Duration::from_millis(timeout_ms).into_nanos(),
        );
        match executor.run_until_stalled(&mut fut) {
            Poll::Ready(x) => break x,
            Poll::Pending => executor.set_fake_time(new_time),
        }
    }
}

// Tests the broadcast functionality. Ensures all non-sending, addressable
// messengers receive a broadcast message.
#[fuchsia::test(allow_stalls = false)]
async fn test_broadcast() {
    let delegate = test::MessageHub::create();

    let (messenger_client_1, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    let (_, mut receptor_2) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(2))).await.unwrap();
    let (_, mut receptor_3) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(3))).await.unwrap();

    let _ = messenger_client_1.message(ORIGINAL.clone(), Audience::Broadcast);

    verify_payload(ORIGINAL.clone(), &mut receptor_2, None).await;
    verify_payload(ORIGINAL.clone(), &mut receptor_3, None).await;
}

// Verifies delivery statuses are properly relayed back to the original sender.
#[fuchsia::test(allow_stalls = false)]
async fn test_delivery_status() {
    let delegate = test::MessageHub::create();
    let known_receiver_address = crate::Address::Test(2);
    let unknown_address = crate::Address::Test(3);
    let (messenger_client_1, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    let (_, mut receptor_2) =
        delegate.create(MessengerType::Addressable(known_receiver_address)).await.unwrap();

    {
        let mut receptor =
            messenger_client_1.message(ORIGINAL.clone(), Audience::Address(known_receiver_address));

        // Ensure observer gets payload and then do nothing with message.
        verify_payload(ORIGINAL.clone(), &mut receptor_2, None).await;

        verify_result(Status::Received, &mut receptor).await;
    }

    {
        let mut receptor =
            messenger_client_1.message(ORIGINAL.clone(), Audience::Address(unknown_address));

        verify_result(Status::Undeliverable, &mut receptor).await;
    }
}

// Verifies message is delivered even if messenger is deleted right
// after.
#[fuchsia::test(allow_stalls = false)]
async fn test_send_delete() {
    let delegate = test::MessageHub::create();
    let (_, mut receptor_2) = delegate
        .create(MessengerType::Addressable(crate::Address::Test(2)))
        .await
        .expect("client should be created");
    {
        let (messenger_client_1, _) =
            delegate.create(MessengerType::Unbound).await.expect("client should be created");
        let _ = messenger_client_1.message(ORIGINAL.clone(), Audience::Broadcast);
    }

    // Ensure observer gets payload and then do nothing with message.
    verify_payload(ORIGINAL.clone(), &mut receptor_2, None).await;
}

// Verifies beacon returns error when receptor goes out of scope.
#[fuchsia::test(allow_stalls = false)]
async fn test_beacon_error() {
    let delegate = test::MessageHub::create();

    let (messenger_client, _) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();
    {
        let (_, mut receptor) =
            delegate.create(MessengerType::Addressable(crate::Address::Test(2))).await.unwrap();

        verify_result(
            Status::Received,
            &mut messenger_client
                .message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(2))),
        )
        .await;
        verify_payload(ORIGINAL.clone(), &mut receptor, None).await;
    }

    verify_result(
        Status::Undeliverable,
        &mut messenger_client.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(2))),
    )
    .await;
}

// Verifies Acknowledge is fully passed back.
#[fuchsia::test(allow_stalls = false)]
async fn test_acknowledge() {
    let delegate = test::MessageHub::create();

    let (_, mut receptor) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();

    let (messenger, _) = delegate.create(MessengerType::Unbound).await.unwrap();

    let mut message_receptor =
        messenger.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(1)));

    verify_payload(ORIGINAL.clone(), &mut receptor, None).await;

    assert!(message_receptor.wait_for_acknowledge().await.is_ok());
}

// Verifies observers can participate in messaging.
#[fuchsia::test(allow_stalls = false)]
async fn test_messenger_behavior() {
    // Run tests twice to ensure no one instance leads to a deadlock.
    for _ in 0..2 {
        verify_messenger_behavior(MessengerType::Broker(no_filter())).await;
        verify_messenger_behavior(MessengerType::Unbound).await;
        verify_messenger_behavior(MessengerType::Addressable(crate::Address::Test(2))).await;
    }
}

async fn verify_messenger_behavior(messenger_type: MessengerType) {
    let delegate = test::MessageHub::create();

    // Messenger to receive message.
    let (target_client, mut target_receptor) =
        delegate.create(MessengerType::Addressable(crate::Address::Test(1))).await.unwrap();

    // Author Messenger.
    let (test_client, mut test_receptor) = delegate.create(messenger_type).await.unwrap();

    // Send top level message from the Messenger.
    let mut reply_receptor =
        test_client.message(ORIGINAL.clone(), Audience::Address(crate::Address::Test(1)));

    let captured_signature = Arc::new(Mutex::new(None));

    // Verify target messenger received message and capture Signature.
    verify_payload(ORIGINAL.clone(), &mut target_receptor, {
        let captured_signature = captured_signature.clone();
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let mut author = captured_signature.lock().await;
                *author = Some(client.get_author());
                let _ = client.reply(REPLY.clone());
            })
        }))
    })
    .await;

    // Verify messenger received reply on the message receptor.
    verify_payload(REPLY.clone(), &mut reply_receptor, None).await;

    let messenger_signature =
        captured_signature.lock().await.take().expect("signature should be populated");

    // Send top level message to Messenger.
    let _ = target_client.message(ORIGINAL.clone(), Audience::Messenger(messenger_signature));

    // Verify Messenger received message.
    verify_payload(ORIGINAL.clone(), &mut test_receptor, None).await;
}

// Ensures unbound messengers operate properly
#[fuchsia::test(allow_stalls = false)]
async fn test_unbound_messenger() {
    let delegate = test::MessageHub::create();

    let (unbound_messenger_1, _) = delegate.create(MessengerType::Unbound).await.unwrap();

    let (_, mut unbound_receptor) =
        delegate.create(MessengerType::Unbound).await.expect("messenger should be created");

    let mut reply_receptor = unbound_messenger_1
        .message(ORIGINAL.clone(), Audience::Messenger(unbound_receptor.get_signature()));

    // Verify target messenger received message and send response.
    verify_payload(
        ORIGINAL.clone(),
        &mut unbound_receptor,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    verify_payload(REPLY.clone(), &mut reply_receptor, None).await;
}

// Ensures next_payload returns the correct values.
#[fuchsia::test(allow_stalls = false)]
async fn test_next_payload() {
    let delegate = test::MessageHub::create();
    let (unbound_messenger_1, _) = delegate.create(MessengerType::Unbound).await.unwrap();

    let (_, mut unbound_receptor_2) =
        delegate.create(MessengerType::Unbound).await.expect("should create messenger");

    let _ = unbound_messenger_1
        .message(ORIGINAL.clone(), Audience::Messenger(unbound_receptor_2.get_signature()));

    let receptor_result = unbound_receptor_2.next_payload().await;

    let (payload, _) = receptor_result.unwrap();
    assert_eq!(payload, ORIGINAL.clone());

    {
        let mut receptor =
            unbound_messenger_1.message(REPLY.clone(), Audience::Address(crate::Address::Test(1)));
        // Should return an error
        let receptor_result = receptor.next_payload().await;
        assert!(receptor_result.is_err());
    }
}

// Exercises basic action fuse behavior.
#[fuchsia::test(allow_stalls = false)]
async fn test_action_fuse() {
    // Channel to send the message from the fuse.
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

    {
        let _ = ActionFuse::create(Box::new(move || {
            tx.unbounded_send(()).unwrap();
        }));
    }

    assert!(rx.next().await.is_some());
}

// Verifies that the proper signal is fired when a receptor disappears.
#[fuchsia::test(allow_stalls = false)]
async fn test_bind_to_recipient() {
    let delegate = test::MessageHub::create();
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

    let (_, mut receptor) =
        delegate.create(MessengerType::Unbound).await.expect("should create messenger");

    {
        let (scoped_messenger, _scoped_receptor) =
            delegate.create(MessengerType::Unbound).await.unwrap();
        let _ = scoped_messenger
            .message(ORIGINAL.clone(), Audience::Messenger(receptor.get_signature()));

        if let Some(MessageEvent::Message(payload, mut client)) = receptor.next().await {
            assert_eq!(payload, ORIGINAL.clone());
            client
                .bind_to_recipient(ActionFuse::create(Box::new(move || {
                    tx.unbounded_send(()).unwrap();
                })))
                .await;
        } else {
            panic!("Should have received message");
        }
    }

    // Receptor has fallen out of scope, should receive callback.
    assert!(rx.next().await.is_some());
}

#[fuchsia::test(allow_stalls = false)]
async fn test_reply_propagation() {
    let delegate = test::MessageHub::create();

    // Create messenger to send source message.
    let (sending_messenger, _) =
        delegate.create(MessengerType::Unbound).await.expect("sending messenger should be created");

    // Create broker to propagate a derived message.
    let (_, mut broker) = delegate
        .create(MessengerType::Broker(Arc::new(move |message| *message.payload() == *REPLY)))
        .await
        .expect("broker should be created");

    // Create messenger to be target of source message.
    let (_, mut target_receptor) =
        delegate.create(MessengerType::Unbound).await.expect("target messenger should be created");

    // Send top level message.
    let mut result_receptor = sending_messenger
        .message(ORIGINAL.clone(), Audience::Messenger(target_receptor.get_signature()));

    // Ensure target receives message and reply back.
    verify_payload(
        ORIGINAL.clone(),
        &mut target_receptor,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    // Ensure broker receives reply and propagate modified message.
    verify_payload(
        REPLY.clone(),
        &mut broker,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.propagate(MODIFIED.clone());
            })
        })),
    )
    .await;

    // Ensure original sender gets reply.
    verify_payload(MODIFIED.clone(), &mut result_receptor, None).await;
}

#[fuchsia::test(allow_stalls = false)]
async fn test_propagation() {
    let delegate = test::MessageHub::create();

    // Create messenger to send source message.
    let (sending_messenger, sending_receptor) =
        delegate.create(MessengerType::Unbound).await.expect("sending messenger should be created");
    let sending_signature = sending_receptor.get_signature();

    // Create brokers to propagate a derived message.
    let (_, mut broker_1) = delegate
        .create(MessengerType::Broker(no_filter()))
        .await
        .expect("broker should be created");
    let modifier_1_signature = broker_1.get_signature();

    let (_, mut broker_2) = delegate
        .create(MessengerType::Broker(no_filter()))
        .await
        .expect("broker should be created");
    let modifier_2_signature = broker_2.get_signature();

    // Create messenger to be target of source message.
    let (_, mut target_receptor) =
        delegate.create(MessengerType::Unbound).await.expect("target messenger should be created");

    // Send top level message.
    let mut result_receptor = sending_messenger
        .message(ORIGINAL.clone(), Audience::Messenger(target_receptor.get_signature()));

    // Ensure broker 1 receives original message and propagate modified message.
    verify_payload(
        ORIGINAL.clone(),
        &mut broker_1,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.propagate(MODIFIED.clone());
            })
        })),
    )
    .await;

    // Ensure broker 2 receives modified message and propagates a differen
    // modified message.
    verify_payload(
        MODIFIED.clone(),
        &mut broker_2,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = client.propagate(MODIFIED_2.clone());
            })
        })),
    )
    .await;

    // Ensure target receives message and reply back.
    verify_payload(
        MODIFIED_2.clone(),
        &mut target_receptor,
        Some(Box::new(move |client| -> BoxFuture<'_, ()> {
            Box::pin(async move {
                // ensure the original author is attributed to the message.
                assert_eq!(client.get_author(), sending_signature);
                // ensure the modifiers are present.
                assert!(client.get_modifiers().contains(&modifier_1_signature));
                assert!(client.get_modifiers().contains(&modifier_2_signature));
                // ensure the message author has not been modified.
                let _ = client.reply(REPLY.clone());
            })
        })),
    )
    .await;

    // Ensure original sender gets reply.
    verify_payload(REPLY.clone(), &mut result_receptor, None).await;
}

#[fuchsia::test(allow_stalls = false)]
async fn test_broker_filter_custom() {
    // Prepare a message hub with a sender, broker, and target.
    let delegate = test::MessageHub::create();

    // Messenger to send broadcast message and targeted message.
    let (messenger, _) = delegate
        .create(MessengerType::Unbound)
        .await
        .expect("broadcast messenger should be created");
    // Filter to target only the ORIGINAL message.
    let filter: Filter = Arc::new(|message| *message.payload() == *ORIGINAL);
    // Broker that should only target ORIGINAL messages.
    let (_, mut broker_receptor) =
        delegate.create(MessengerType::Broker(filter)).await.expect("broker should be created");

    // Send broadcast message.
    let _ = messenger.message(BROADCAST.clone(), Audience::Broadcast);

    // Send original message.
    let _ = messenger.message(ORIGINAL.clone(), Audience::Broadcast);
    // Ensure broker gets message. If the broadcast message was received, this
    // will fail.
    verify_payload(ORIGINAL.clone(), &mut broker_receptor, None).await;
}

// Verify that using a closure that captures a variable for a custom filter works, since it can't
// be used in place of an function pointer.
#[fuchsia::test(allow_stalls = false)]
async fn test_broker_filter_caputring_closure() {
    // Prepare a message hub with a sender, broker, and target.
    let delegate = test::MessageHub::create();

    // Messenger to send broadcast message and targeted message.
    let (messenger, _) = delegate
        .create(MessengerType::Unbound)
        .await
        .expect("broadcast messenger should be created");
    // Filter to target only the Foo message.
    let expected_payload = &test_message::FOO;
    let filter: Filter = Arc::new(move |message| *message.payload() == *expected_payload);
    // Broker that should only target Foo messages.
    let (_, mut broker_receptor) =
        delegate.create(MessengerType::Broker(filter)).await.expect("broker should be created");

    // Send broadcast message.
    let _ = messenger.message(BROADCAST.clone(), Audience::Broadcast);

    // Send foo message.
    let _ = messenger.message(expected_payload.clone(), Audience::Broadcast);
    // Ensure broker gets message. If the broadcast message was received, this
    // will fail.
    verify_payload(expected_payload.clone(), &mut broker_receptor, None).await;
}

#[fuchsia::test(allow_stalls = false)]
async fn test_audience_matching() {
    let target_audience = Audience::Address(crate::Address::Test(1));
    // An audience should contain itself.
    assert!(target_audience == target_audience);
    // An audience with only broadcast should not match.
    #[allow(clippy::bool_assert_comparison)]
    {
        let audience = Audience::Broadcast;
        assert_eq!(audience == target_audience, false);
    }
}

// Ensures all members of a role receive messages.
#[fuchsia::test(allow_stalls = false)]
async fn test_roles_membership() {
    // Prepare a message hub.
    let delegate = test::MessageHub::create();

    // Create messengers who participate in roles
    let (_, mut foo_role_receptor) =
        delegate.create_sink().await.expect("recipient messenger should be created");
    let (_, mut foo_role_receptor_2) =
        delegate.create_sink().await.expect("recipient messenger should be created");

    // Create messenger to send a message to the given participant.
    let (sender, _) =
        delegate.create(MessengerType::Unbound).await.expect("sending messenger should be created");

    let message = test_message::FOO.clone();
    let audience = Audience::EventSink;
    let _ = sender.message(message.clone(), audience);

    // Verify payload received by role members.
    verify_payload(message.clone(), &mut foo_role_receptor, None).await;
    verify_payload(message, &mut foo_role_receptor_2, None).await;
}

// Ensures only role members receive messages directed to the role.
#[fuchsia::test(allow_stalls = false)]
async fn test_roles_audience() {
    // Prepare a message hub.
    let delegate = test::MessageHub::create();

    // Create messenger who participate in a role
    let (_, mut foo_role_receptor) =
        delegate.create_sink().await.expect("recipient messenger should be created");

    // Create another messenger with no role to ensure messages are not routed
    // improperly to other messengers.
    let (_, mut outside_receptor) =
        delegate.create(MessengerType::Unbound).await.expect("other messenger should be created");
    let outside_signature = outside_receptor.get_signature();

    // Create messenger to send a message to the given participant.
    let (sender, _) =
        delegate.create(MessengerType::Unbound).await.expect("sending messenger should be created");

    // Send message to role.
    {
        let message = test_message::FOO.clone();
        let audience = Audience::EventSink;
        let _ = sender.message(message.clone(), audience);

        // Verify payload received by role members.
        verify_payload(message, &mut foo_role_receptor, None).await;
    }

    // Send message to outside messenger.
    {
        let message = test_message::BAZ.clone();
        let audience = Audience::Messenger(outside_signature);
        let _ = sender.message(message.clone(), audience);

        // Since outside messenger isn't part of the role, the next message should
        // be the one sent directly to it, rather than the role.
        verify_payload(message, &mut outside_receptor, None).await;
    }
}
