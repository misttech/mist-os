// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use futures::StreamExt;
use std::collections::HashMap;
use std::rc::Rc;

use crate::Error;

/// Contains all the data received over a `SnapshotReceiver` channel.
#[derive(Debug)]
pub struct Snapshot {
    /// All the live allocations in the analyzed process, indexed by memory address.
    pub allocations: HashMap<u64, Allocation>,

    /// All the executable memory regions in the analyzed process, indexed by start address.
    pub executable_regions: HashMap<u64, ExecutableRegion>,
}

/// Information about an allocated memory block and, optionally, its contents.
#[derive(Debug)]
pub struct Allocation {
    /// Block size, in bytes.
    pub size: u64,

    /// The allocating thread.
    pub thread_info: Rc<ThreadInfo>,

    /// The stack trace of the allocation site.
    pub stack_trace: Rc<StackTrace>,

    /// Allocation timestamp, in nanoseconds.
    pub timestamp: i64,

    /// Memory dump of this block's contents.
    pub contents: Option<Vec<u8>>,
}

/// A stack trace.
#[derive(Debug)]
pub struct StackTrace {
    /// Code addresses at each call frame. The first entry corresponds to the leaf call.
    pub program_addresses: Vec<u64>,
}

/// A memory region containing code loaded from an ELF file.
#[derive(Debug)]
pub struct ExecutableRegion {
    /// Region size, in bytes.
    pub size: u64,

    /// The corresponding offset in the ELF file.
    pub file_offset: u64,

    /// The Build ID of the ELF file.
    pub build_id: Vec<u8>,
}

/// Information identifying a specific thread.
#[derive(Debug)]
pub struct ThreadInfo {
    /// The thread's koid.
    pub koid: fuchsia_zircon_types::zx_koid_t,

    /// The thread's name.
    pub name: String,
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
        let mut allocations: HashMap<u64, (u64, u64, u64, i64)> = HashMap::new();
        let mut thread_infos: HashMap<u64, Rc<ThreadInfo>> = HashMap::new();
        let mut stack_traces: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut executable_regions: HashMap<u64, ExecutableRegion> = HashMap::new();
        let mut contents: HashMap<u64, Vec<u8>> = HashMap::new();

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
                            let size = read_field!(allocation => Allocation, size)?;
                            let timestamp = read_field!(allocation => Allocation, timestamp)?;
                            let thread_info_key =
                                read_field!(allocation => Allocation, thread_info_key)?;
                            let stack_trace_key =
                                read_field!(allocation => Allocation, stack_trace_key)?;
                            if allocations
                                .insert(
                                    address,
                                    (size, thread_info_key, stack_trace_key, timestamp),
                                )
                                .is_some()
                            {
                                return Err(Error::ConflictingElement {
                                    element_type: "Allocation",
                                });
                            }
                        }
                        fheapdump_client::SnapshotElement::StackTrace(stack_trace) => {
                            let stack_trace_key =
                                read_field!(stack_trace => StackTrace, stack_trace_key)?;
                            let mut program_addresses =
                                read_field!(stack_trace => StackTrace, program_addresses)?;
                            stack_traces
                                .entry(stack_trace_key)
                                .or_default()
                                .append(&mut program_addresses);
                        }
                        fheapdump_client::SnapshotElement::ThreadInfo(thread_info) => {
                            let thread_info_key =
                                read_field!(thread_info => ThreadInfo, thread_info_key)?;
                            let koid = read_field!(thread_info => ThreadInfo, koid)?;
                            let name = read_field!(thread_info => ThreadInfo, name)?;
                            if thread_infos
                                .insert(thread_info_key, Rc::new(ThreadInfo { koid, name }))
                                .is_some()
                            {
                                return Err(Error::ConflictingElement {
                                    element_type: "ThreadInfo",
                                });
                            }
                        }
                        fheapdump_client::SnapshotElement::ExecutableRegion(region) => {
                            let address = read_field!(region => ExecutableRegion, address)?;
                            let size = read_field!(region => ExecutableRegion, size)?;
                            let file_offset = read_field!(region => ExecutableRegion, file_offset)?;
                            let build_id = read_field!(region => ExecutableRegion, build_id)?.value;
                            let region = ExecutableRegion { size, file_offset, build_id };
                            if executable_regions.insert(address, region).is_some() {
                                return Err(Error::ConflictingElement {
                                    element_type: "ExecutableRegion",
                                });
                            }
                        }
                        fheapdump_client::SnapshotElement::BlockContents(block_contents) => {
                            let address = read_field!(block_contents => BlockContents, address)?;
                            let mut chunk = read_field!(block_contents => BlockContents, contents)?;
                            contents.entry(address).or_default().append(&mut chunk);
                        }
                        _ => return Err(Error::UnexpectedElementType),
                    }
                }
            } else {
                // We are at the end of the stream. Convert to the final types and resolve
                // cross-references.
                let final_stack_traces: HashMap<u64, Rc<StackTrace>> = stack_traces
                    .into_iter()
                    .map(|(key, program_addresses)| {
                        (key, Rc::new(StackTrace { program_addresses }))
                    })
                    .collect();
                let mut final_allocations = HashMap::new();
                for (address, (size, thread_info_key, stack_trace_key, timestamp)) in allocations {
                    let thread_info = thread_infos
                        .get(&thread_info_key)
                        .ok_or(Error::InvalidCrossReference { element_type: "ThreadInfo" })?
                        .clone();
                    let stack_trace = final_stack_traces
                        .get(&stack_trace_key)
                        .ok_or(Error::InvalidCrossReference { element_type: "StackTrace" })?
                        .clone();
                    let contents = match contents.remove(&address) {
                        Some(data) if data.len() as u64 != size => {
                            return Err(Error::ConflictingElement { element_type: "BlockContents" })
                        }
                        other => other,
                    };
                    final_allocations.insert(
                        address,
                        Allocation { size, thread_info, stack_trace, timestamp, contents },
                    );
                }

                return Ok(Snapshot { allocations: final_allocations, executable_regions });
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
    const FAKE_ALLOCATION_1_TIMESTAMP: i64 = 888888888;
    const FAKE_ALLOCATION_1_CONTENTS: [u8; FAKE_ALLOCATION_1_SIZE as usize] = *b"12345678";
    const FAKE_ALLOCATION_2_ADDRESS: u64 = 5678;
    const FAKE_ALLOCATION_2_SIZE: u64 = 4;
    const FAKE_ALLOCATION_2_TIMESTAMP: i64 = -777777777; // test negative value too
    const FAKE_THREAD_1_KOID: u64 = 1212;
    const FAKE_THREAD_1_NAME: &str = "fake-thread-1-name";
    const FAKE_THREAD_1_KEY: u64 = 4567;
    const FAKE_THREAD_2_KOID: u64 = 1213;
    const FAKE_THREAD_2_NAME: &str = "fake-thread-2-name";
    const FAKE_THREAD_2_KEY: u64 = 7654;
    const FAKE_STACK_TRACE_1_ADDRESSES: [u64; 6] = [11111, 22222, 33333, 22222, 44444, 55555];
    const FAKE_STACK_TRACE_1_KEY: u64 = 9876;
    const FAKE_STACK_TRACE_2_ADDRESSES: [u64; 4] = [11111, 22222, 11111, 66666];
    const FAKE_STACK_TRACE_2_KEY: u64 = 6789;
    const FAKE_REGION_1_ADDRESS: u64 = 0x10000000;
    const FAKE_REGION_1_SIZE: u64 = 0x80000;
    const FAKE_REGION_1_FILE_OFFSET: u64 = 0x1000;
    const FAKE_REGION_1_BUILD_ID: &[u8] = &[0xaa; 20];
    const FAKE_REGION_2_ADDRESS: u64 = 0x7654300000;
    const FAKE_REGION_2_SIZE: u64 = 0x200000;
    const FAKE_REGION_2_FILE_OFFSET: u64 = 0x2000;
    const FAKE_REGION_2_BUILD_ID: &[u8] = &[0x55; 32];

    #[fasync::run_singlethreaded(test)]
    async fn test_empty() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify that it is empty.
        let received_snapshot = receive_worker.await.unwrap();
        assert!(received_snapshot.allocations.is_empty());
        assert!(received_snapshot.executable_regions.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_one_batch() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send a batch containing two allocations - whose threads, stack traces and contents can be
        // listed before or after the allocation(s) that reference them - and two executable
        // regions.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::BlockContents(fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(FAKE_ALLOCATION_1_CONTENTS.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_1_ADDRESS),
                    size: Some(FAKE_REGION_1_SIZE),
                    file_offset: Some(FAKE_REGION_1_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_1_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_2_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_2_ADDRESS),
                    size: Some(FAKE_REGION_2_SIZE),
                    file_offset: Some(FAKE_REGION_2_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_2_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_2_KEY),
                koid: Some(FAKE_THREAD_2_KOID),
                name: Some(FAKE_THREAD_2_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_2_ADDRESS),
                size: Some(FAKE_ALLOCATION_2_SIZE),
                thread_info_key: Some(FAKE_THREAD_2_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_2_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_2_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_2_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify its contents.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.size, FAKE_ALLOCATION_1_SIZE);
        assert_eq!(allocation1.thread_info.koid, FAKE_THREAD_1_KOID);
        assert_eq!(allocation1.thread_info.name, FAKE_THREAD_1_NAME);
        assert_eq!(allocation1.stack_trace.program_addresses, FAKE_STACK_TRACE_2_ADDRESSES);
        assert_eq!(allocation1.timestamp, FAKE_ALLOCATION_1_TIMESTAMP);
        assert_eq!(allocation1.contents.expect("contents must be set"), FAKE_ALLOCATION_1_CONTENTS);
        let allocation2 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_2_ADDRESS).unwrap();
        assert_eq!(allocation2.size, FAKE_ALLOCATION_2_SIZE);
        assert_eq!(allocation2.thread_info.koid, FAKE_THREAD_2_KOID);
        assert_eq!(allocation2.thread_info.name, FAKE_THREAD_2_NAME);
        assert_eq!(allocation2.stack_trace.program_addresses, FAKE_STACK_TRACE_1_ADDRESSES);
        assert_eq!(allocation2.timestamp, FAKE_ALLOCATION_2_TIMESTAMP);
        assert_matches!(allocation2.contents, None, "no contents are sent for this allocation");
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
        let region1 = received_snapshot.executable_regions.remove(&FAKE_REGION_1_ADDRESS).unwrap();
        assert_eq!(region1.size, FAKE_REGION_1_SIZE);
        assert_eq!(region1.file_offset, FAKE_REGION_1_FILE_OFFSET);
        assert_eq!(region1.build_id, FAKE_REGION_1_BUILD_ID);
        let region2 = received_snapshot.executable_regions.remove(&FAKE_REGION_2_ADDRESS).unwrap();
        assert_eq!(region2.size, FAKE_REGION_2_SIZE);
        assert_eq!(region2.file_offset, FAKE_REGION_2_FILE_OFFSET);
        assert_eq!(region2.build_id, FAKE_REGION_2_BUILD_ID);
        assert!(received_snapshot.executable_regions.is_empty(), "all entries have been removed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_two_batches() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send a first batch.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_2_ADDRESS),
                    size: Some(FAKE_REGION_2_SIZE),
                    file_offset: Some(FAKE_REGION_2_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_2_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_2_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_2_KEY),
                koid: Some(FAKE_THREAD_2_KOID),
                name: Some(FAKE_THREAD_2_NAME.to_string()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send another batch.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_2_ADDRESS),
                size: Some(FAKE_ALLOCATION_2_SIZE),
                thread_info_key: Some(FAKE_THREAD_2_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_2_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_1_ADDRESS),
                    size: Some(FAKE_REGION_1_SIZE),
                    file_offset: Some(FAKE_REGION_1_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_1_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_2_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_2_ADDRESSES.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::BlockContents(fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(FAKE_ALLOCATION_1_CONTENTS.to_vec()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Receive the snapshot we just transmitted and verify its contents.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.size, FAKE_ALLOCATION_1_SIZE);
        assert_eq!(allocation1.thread_info.koid, FAKE_THREAD_1_KOID);
        assert_eq!(allocation1.thread_info.name, FAKE_THREAD_1_NAME);
        assert_eq!(allocation1.stack_trace.program_addresses, FAKE_STACK_TRACE_2_ADDRESSES);
        assert_eq!(allocation1.timestamp, FAKE_ALLOCATION_1_TIMESTAMP);
        assert_eq!(allocation1.contents.expect("contents must be set"), FAKE_ALLOCATION_1_CONTENTS);
        let allocation2 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_2_ADDRESS).unwrap();
        assert_eq!(allocation2.size, FAKE_ALLOCATION_2_SIZE);
        assert_eq!(allocation2.thread_info.koid, FAKE_THREAD_2_KOID);
        assert_eq!(allocation2.thread_info.name, FAKE_THREAD_2_NAME);
        assert_eq!(allocation2.stack_trace.program_addresses, FAKE_STACK_TRACE_1_ADDRESSES);
        assert_eq!(allocation2.timestamp, FAKE_ALLOCATION_2_TIMESTAMP);
        assert_matches!(allocation2.contents, None, "no contents are sent for this allocation");
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
        let region1 = received_snapshot.executable_regions.remove(&FAKE_REGION_1_ADDRESS).unwrap();
        assert_eq!(region1.size, FAKE_REGION_1_SIZE);
        assert_eq!(region1.file_offset, FAKE_REGION_1_FILE_OFFSET);
        assert_eq!(region1.build_id, FAKE_REGION_1_BUILD_ID);
        let region2 = received_snapshot.executable_regions.remove(&FAKE_REGION_2_ADDRESS).unwrap();
        assert_eq!(region2.size, FAKE_REGION_2_SIZE);
        assert_eq!(region2.file_offset, FAKE_REGION_2_FILE_OFFSET);
        assert_eq!(region2.build_id, FAKE_REGION_2_BUILD_ID);
        assert!(received_snapshot.executable_regions.is_empty(), "all entries have been removed");
    }

    #[test_case(|allocation| allocation.address = None => matches
        Err(Error::MissingField { container: "Allocation", field: "address" }) ; "address")]
    #[test_case(|allocation| allocation.size = None => matches
        Err(Error::MissingField { container: "Allocation", field: "size" }) ; "size")]
    #[test_case(|allocation| allocation.thread_info_key = None => matches
        Err(Error::MissingField { container: "Allocation", field: "thread_info_key" }) ; "thread_info_key")]
    #[test_case(|allocation| allocation.stack_trace_key = None => matches
        Err(Error::MissingField { container: "Allocation", field: "stack_trace_key" }) ; "stack_trace_key")]
    #[test_case(|allocation| allocation.timestamp = None => matches
        Err(Error::MissingField { container: "Allocation", field: "timestamp" }) ; "timestamp")]
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
            thread_info_key: Some(FAKE_THREAD_1_KEY),
            stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
            timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
            ..Default::default()
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut allocation);

        // Send it to the SnapshotReceiver along with the thread info and stack trace it references.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(allocation),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[test_case(|thread_info| thread_info.thread_info_key = None => matches
        Err(Error::MissingField { container: "ThreadInfo", field: "thread_info_key" }) ; "thread_info_key")]
    #[test_case(|thread_info| thread_info.koid = None => matches
        Err(Error::MissingField { container: "ThreadInfo", field: "koid" }) ; "koid")]
    #[test_case(|thread_info| thread_info.name = None => matches
        Err(Error::MissingField { container: "ThreadInfo", field: "name" }) ; "name")]
    #[test_case(|_| () /* if we do not set any field to None, the result should be Ok */ => matches
        Ok(_) ; "success")]
    #[fasync::run_singlethreaded(test)]
    async fn test_thread_info_required_fields(
        set_one_field_to_none: fn(&mut fheapdump_client::ThreadInfo),
    ) -> Result<Snapshot, Error> {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Start with a ThreadInfo with all the required fields set.
        let mut thread_info = fheapdump_client::ThreadInfo {
            thread_info_key: Some(FAKE_THREAD_1_KEY),
            koid: Some(FAKE_THREAD_1_KOID),
            name: Some(FAKE_THREAD_1_NAME.to_string()),
            ..Default::default()
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut thread_info);

        // Send it to the SnapshotReceiver.
        let fut =
            receiver_proxy.batch(&[fheapdump_client::SnapshotElement::ThreadInfo(thread_info)]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[test_case(|stack_trace| stack_trace.stack_trace_key = None => matches
        Err(Error::MissingField { container: "StackTrace", field: "stack_trace_key" }) ; "stack_trace_key")]
    #[test_case(|stack_trace| stack_trace.program_addresses = None => matches
        Err(Error::MissingField { container: "StackTrace", field: "program_addresses" }) ; "program_addresses")]
    #[test_case(|_| () /* if we do not set any field to None, the result should be Ok */ => matches
        Ok(_) ; "success")]
    #[fasync::run_singlethreaded(test)]
    async fn test_stack_trace_required_fields(
        set_one_field_to_none: fn(&mut fheapdump_client::StackTrace),
    ) -> Result<Snapshot, Error> {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Start with a StackTrace with all the required fields set.
        let mut stack_trace = fheapdump_client::StackTrace {
            stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
            program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
            ..Default::default()
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut stack_trace);

        // Send it to the SnapshotReceiver.
        let fut =
            receiver_proxy.batch(&[fheapdump_client::SnapshotElement::StackTrace(stack_trace)]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[test_case(|region| region.address = None => matches
        Err(Error::MissingField { container: "ExecutableRegion", field: "address" }) ; "address")]
    #[test_case(|region| region.size = None => matches
        Err(Error::MissingField { container: "ExecutableRegion", field: "size" }) ; "size")]
    #[test_case(|region| region.file_offset = None => matches
        Err(Error::MissingField { container: "ExecutableRegion", field: "file_offset" }) ; "file_offset")]
    #[test_case(|region| region.build_id = None => matches
        Err(Error::MissingField { container: "ExecutableRegion", field: "build_id" }) ; "build_id")]
    #[test_case(|_| () /* if we do not set any field to None, the result should be Ok */ => matches
        Ok(_) ; "success")]
    #[fasync::run_singlethreaded(test)]
    async fn test_executable_region_required_fields(
        set_one_field_to_none: fn(&mut fheapdump_client::ExecutableRegion),
    ) -> Result<Snapshot, Error> {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Start with an ExecutableRegion with all the required fields set.
        let mut region = fheapdump_client::ExecutableRegion {
            address: Some(FAKE_REGION_1_ADDRESS),
            size: Some(FAKE_REGION_1_SIZE),
            file_offset: Some(FAKE_REGION_1_FILE_OFFSET),
            build_id: Some(fheapdump_client::BuildId { value: FAKE_REGION_1_BUILD_ID.to_vec() }),
            ..Default::default()
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut region);

        // Send it to the SnapshotReceiver.
        let fut =
            receiver_proxy.batch(&[fheapdump_client::SnapshotElement::ExecutableRegion(region)]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[test_case(|block_contents| block_contents.address = None => matches
        Err(Error::MissingField { container: "BlockContents", field: "address" }) ; "address")]
    #[test_case(|block_contents| block_contents.contents = None => matches
        Err(Error::MissingField { container: "BlockContents", field: "contents" }) ; "contents")]
    #[test_case(|_| () /* if we do not set any field to None, the result should be Ok */ => matches
        Ok(_) ; "success")]
    #[fasync::run_singlethreaded(test)]
    async fn test_block_contents_required_fields(
        set_one_field_to_none: fn(&mut fheapdump_client::BlockContents),
    ) -> Result<Snapshot, Error> {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Start with a BlockContents with all the required fields set.
        let mut block_contents = fheapdump_client::BlockContents {
            address: Some(FAKE_ALLOCATION_1_ADDRESS),
            contents: Some(FAKE_ALLOCATION_1_CONTENTS.to_vec()),
            ..Default::default()
        };

        // Set one of the fields to None, according to the case being tested.
        set_one_field_to_none(&mut block_contents);

        // Send it to the SnapshotReceiver along with the allocation it references.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::BlockContents(block_contents),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Return the result.
        receive_worker.await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_conflicting_allocations() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send two allocations with the same address along with the stack trace they reference.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Verify expected error.
        assert_matches!(
            receive_worker.await,
            Err(Error::ConflictingElement { element_type: "Allocation" })
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_conflicting_executable_regions() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send two executable regions with the same address.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_1_ADDRESS),
                    size: Some(FAKE_REGION_1_SIZE),
                    file_offset: Some(FAKE_REGION_1_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_1_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
            fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_1_ADDRESS),
                    size: Some(FAKE_REGION_1_SIZE),
                    file_offset: Some(FAKE_REGION_1_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_1_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ),
        ]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Verify expected error.
        assert_matches!(
            receive_worker.await,
            Err(Error::ConflictingElement { element_type: "ExecutableRegion" })
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_block_contents_wrong_size() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send an allocation whose BlockContents has the wrong size.
        let contents_with_wrong_size = vec![0; FAKE_ALLOCATION_1_SIZE as usize + 1];
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::BlockContents(fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(contents_with_wrong_size),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        let _ = fut.await; // ignore result, as the peer may detect the error and close the channel

        // Verify expected error.
        assert_matches!(
            receive_worker.await,
            Err(Error::ConflictingElement { element_type: "BlockContents" })
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_empty_stack_trace() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send an allocation that references an empty stack trace.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(vec![]),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Verify that the stack trace has been reconstructed correctly.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.stack_trace.program_addresses, []);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_chunked_stack_trace() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send an allocation and the first chunk of its stack trace.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(vec![1111, 2222]),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the second chunk.
        let fut = receiver_proxy.batch(&[fheapdump_client::SnapshotElement::StackTrace(
            fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(vec![3333]),
                ..Default::default()
            },
        )]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Verify that the stack trace has been reconstructed correctly.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.stack_trace.program_addresses, [1111, 2222, 3333]);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_empty_block_contents() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send a zero-sized allocation and its empty contents.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(0),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::BlockContents(fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(vec![]),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Verify that the allocation has been reconstructed correctly.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.contents.expect("contents must be set"), []);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_chunked_block_contents() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Split the contents in two halves.
        let (content_first_chunk, contents_second_chunk) =
            FAKE_ALLOCATION_1_CONTENTS.split_at(FAKE_ALLOCATION_1_CONTENTS.len() / 2);

        // Send an allocation and the first chunk of its contents.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::BlockContents(fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(content_first_chunk.to_vec()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Send the second chunk.
        let fut = receiver_proxy.batch(&[fheapdump_client::SnapshotElement::BlockContents(
            fheapdump_client::BlockContents {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                contents: Some(contents_second_chunk.to_vec()),
                ..Default::default()
            },
        )]);
        fut.await.unwrap();

        // Send the end of stream marker.
        let fut = receiver_proxy.batch(&[]);
        fut.await.unwrap();

        // Verify that the allocation's block contents have been reconstructed correctly.
        let mut received_snapshot = receive_worker.await.unwrap();
        let allocation1 = received_snapshot.allocations.remove(&FAKE_ALLOCATION_1_ADDRESS).unwrap();
        assert_eq!(allocation1.contents.expect("contents must be set"), FAKE_ALLOCATION_1_CONTENTS);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_missing_end_of_stream() {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Send an allocation and its stack trace.
        let fut = receiver_proxy.batch(&[
            fheapdump_client::SnapshotElement::Allocation(fheapdump_client::Allocation {
                address: Some(FAKE_ALLOCATION_1_ADDRESS),
                size: Some(FAKE_ALLOCATION_1_SIZE),
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                timestamp: Some(FAKE_ALLOCATION_1_TIMESTAMP),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::ThreadInfo(fheapdump_client::ThreadInfo {
                thread_info_key: Some(FAKE_THREAD_1_KEY),
                koid: Some(FAKE_THREAD_1_KOID),
                name: Some(FAKE_THREAD_1_NAME.to_string()),
                ..Default::default()
            }),
            fheapdump_client::SnapshotElement::StackTrace(fheapdump_client::StackTrace {
                stack_trace_key: Some(FAKE_STACK_TRACE_1_KEY),
                program_addresses: Some(FAKE_STACK_TRACE_1_ADDRESSES.to_vec()),
                ..Default::default()
            }),
        ]);
        fut.await.unwrap();

        // Close the channel without sending an end of stream marker.
        std::mem::drop(receiver_proxy);

        // Expect an UnexpectedEndOfStream error.
        assert_matches!(receive_worker.await, Err(Error::UnexpectedEndOfStream));
    }
}
