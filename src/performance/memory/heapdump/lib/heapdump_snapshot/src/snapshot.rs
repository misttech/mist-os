// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use futures::StreamExt;
use std::collections::HashMap;

use crate::Error;

/// Contains all the data received over a `SnapshotReceiver` channel.
#[derive(Debug)]
pub struct Snapshot {
    /// All the live allocations in the analyzed process, indexed by memory address.
    pub allocations: HashMap<u64, Allocation>,
}

/// Information about an allocated memory block.
#[derive(Debug)]
pub struct Allocation {
    /// Block size, in bytes.
    pub size: u64,
}

/// Gets the value of a field in a FIDL table as a `Result<T, Error>`.
///
/// An `Err(Error::MissingField { .. })` is returned if the field's value is `None`.
///
/// Usage: `read_field!(container_expression => ContainerType, field_name)`
///
/// # Example
///
/// ```
/// struct MyFidlTable { field: Option<u32>, .. }
/// let table = MyFidlTable { field: Some(44), .. };
///
/// let val = read_field!(table => MyFidlTable, field)?;
/// ```
macro_rules! read_field {
    ($e:expr => $c:ident, $f:ident) => {
        $e.$f.ok_or(Error::MissingField {
            container: std::stringify!($c),
            field: std::stringify!($f),
        })
    };
}

impl Snapshot {
    /// Receives data over a `SnapshotReceiver` channel and reassembles it.
    pub async fn receive_from(
        mut stream: fheapdump_client::SnapshotReceiverRequestStream,
    ) -> Result<Snapshot, Error> {
        let mut allocations = HashMap::new();

        while let Some(fheapdump_client::SnapshotReceiverRequest::Batch {
            batch, responder, ..
        }) = stream.next().await.transpose()?
        {
            // Send acknowledgment.
            responder.send()?;

            // Process data. An empty batch signals the end of the stream.
            if !batch.is_empty() {
                for element in batch {
                    match element {
                        fheapdump_client::SnapshotElement::Allocation(allocation) => {
                            let address = read_field!(allocation => Allocation, address)?;
                            let allocation =
                                Allocation { size: read_field!(allocation => Allocation, size)? };
                            if allocations.insert(address, allocation).is_some() {
                                return Err(Error::ConflictingElement {
                                    element_type: "Allocation",
                                });
                            }
                        }
                        _ => return Err(Error::UnexpectedElementType),
                    }
                }
            } else {
                return Ok(Snapshot { allocations });
            }
        }

        Err(Error::UnexpectedEndOfStream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async as fasync;
    use test_case::test_case;

    // Constants used by some of the tests below:
    const FAKE_ALLOCATION_1_ADDRESS: u64 = 1234;
    const FAKE_ALLOCATION_1_SIZE: u64 = 8;
    const FAKE_ALLOCATION_2_ADDRESS: u64 = 5678;
    const FAKE_ALLOCATION_2_SIZE: u64 = 4;

    #[fasync::run_singlethreaded(test)]
    async fn test_empty() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&mut [].iter_mut());
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify that it is empty.
        let received_snapshot = receive_worker.await.unwrap();
        assert!(received_snapshot.allocations.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_one_batch() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send a batch containing two allocations.
        let fut = receiver_proxy.batch(
            &mut [
                fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                    address: Some(FAKE_ALLOCATION_1_ADDRESS),
                    size: Some(FAKE_ALLOCATION_1_SIZE),
                    ..fheapdump_client::Allocation::EMPTY
                }),
                fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                    address: Some(FAKE_ALLOCATION_2_ADDRESS),
                    size: Some(FAKE_ALLOCATION_2_SIZE),
                    ..fheapdump_client::Allocation::EMPTY
                }),
            ]
            .iter_mut(),
        );
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&mut [].iter_mut());
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify its contents.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.size, FAKE_ALLOCATION_1_SIZE);
        let allocation2 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_2_ADDRESS).unwrap();
        assert_eq!(allocation2.size, FAKE_ALLOCATION_2_SIZE);
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_two_batches() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send a first batch containing an allocation.
        let fut = receiver_proxy.batch(
            &mut [fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                ..fheapdump_client::Allocation::EMPTY
            })]
            .iter_mut(),
        );
        fut.await.unwrap();

        // Send another batch containing another allocation.
        let fut = receiver_proxy.batch(
            &mut [fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_2_ADDRESS),
                size: Some(FAKE_ALLOCATION_2_SIZE),
                ..fheapdump_client::Allocation::EMPTY
            })]
            .iter_mut(),
        );
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&mut [].iter_mut());
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify its contents.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.size, FAKE_ALLOCATION_1_SIZE);
        let allocation2 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_2_ADDRESS).unwrap();
        assert_eq!(allocation2.size, FAKE_ALLOCATION_2_SIZE);
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
    }

    #[test_case(|allocation| allocation.address = None => matches
        Err(Error::MissingField { container: "Allocation", field: "address" }) ; "address")]
    #[test_case(|allocation| allocation.size = None => matches
        Err(Error::MissingField { container: "Allocation", field: "size" }) ; "size")]
    #[test_case(|_| () /* if we do not set any field to None, the result should be Ok */ => matches
        Ok(_) ; "success")]
    #[fasync::run_singlethreaded(test)]
    async fn test_allocation_required_fields(
        set_one_field_to_none: fn(&mut fheapdump_client::Allocation),
    ) -> Result<Snapshot, Error> {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Start with an Allocation with all the required fields set.
        let mut allocation = fheapdump_client::Allocation {
            address: Some(FAKE_ALLOCATION_1_ADDRESS),
            size: Some(FAKE_ALLOCATION_1_SIZE),
            ..fheapdump_client::Allocation::EMPTY
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut allocation);

        // Send it to the SnapshotReceiver.
        let fut = receiver_proxy
            .batch(&mut [fheapdump_client::SnapshotElement::Allocation(allocation)].iter_mut());
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&mut [].iter_mut());
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_conflicting_allocations() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send two allocations with the same address.
        let fut = receiver_proxy.batch(
            &mut [
                fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                    address: Some(FAKE_ALLOCATION_1_ADDRESS),
                    size: Some(FAKE_ALLOCATION_1_SIZE),
                    ..fheapdump_client::Allocation::EMPTY
                }),
                fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                    address: Some(FAKE_ALLOCATION_1_ADDRESS),
                    size: Some(FAKE_ALLOCATION_1_SIZE),
                    ..fheapdump_client::Allocation::EMPTY
                }),
            ]
            .iter_mut(),
        );
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&mut [].iter_mut());
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Verify expected error.
        assert_matches!(
            receive_worker.await,
            Err(Error::ConflictingElement { element_type: "Allocation" })
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_missing_end_of_stream() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send an allocation.
        let fut = receiver_proxy.batch(
            &mut [fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                ..fheapdump_client::Allocation::EMPTY
            })]
            .iter_mut(),
        );
        fut.await.unwrap();

        // Close the channel without sending an end of stream marker.
        std::mem::drop(receiver_proxy);

        // Expect an UnexpectedEndOfStream error.
        assert_matches!(receive_worker.await, Err(Error::UnexpectedEndOfStream));
    }
}
