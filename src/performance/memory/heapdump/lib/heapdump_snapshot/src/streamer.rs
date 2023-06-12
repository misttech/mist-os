// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use fuchsia_zircon_types::ZX_CHANNEL_MAX_MSG_BYTES;
use measure_tape_for_snapshot_element::Measurable;

use crate::Error;

// Number of bytes the header of a vector occupies in a fidl message.
// TODO(fxbug.dev/98653): This should be a constant in a FIDL library.
const FIDL_VECTOR_HEADER_BYTES: usize = 16;

// Number of bytes the header of a fidl message occupies.
// TODO(fxbug.dev/98653): This should be a constant in a FIDL library.
const FIDL_HEADER_BYTES: usize = 16;

// Size of the fixed part of a `SnapshotReceiver/Batch` FIDL message. The actual size is given by
// this number plus the size of each element in the batch.
const EMPTY_BUFFER_SIZE: usize = FIDL_HEADER_BYTES + FIDL_VECTOR_HEADER_BYTES;

/// Implements pagination on top of a SnapshotReceiver channel.
pub struct Streamer {
    dest: fheapdump_client::SnapshotReceiverProxy,
    buffer: Vec<fheapdump_client::SnapshotElement>,
    buffer_size: usize,
}

impl Streamer {
    pub fn new(dest: fheapdump_client::SnapshotReceiverProxy) -> Streamer {
        Streamer { dest, buffer: Vec::new(), buffer_size: EMPTY_BUFFER_SIZE }
    }

    /// Sends the given `elem`.
    ///
    /// This method internally flushes the outgoing buffer, if necessary, so that it never exceeds
    /// the maximum allowed size.
    pub async fn push_element(
        mut self,
        elem: fheapdump_client::SnapshotElement,
    ) -> Result<Streamer, Error> {
        let elem_size = elem.measure().num_bytes;

        // Flush the current buffer if the new element would not fit in it.
        if self.buffer_size + elem_size > ZX_CHANNEL_MAX_MSG_BYTES as usize {
            self.flush_buffer().await?;
        }

        // Append the new element.
        self.buffer.push(elem);
        self.buffer_size += elem_size;

        Ok(self)
    }

    /// Sends the end-of-stream marker.
    pub async fn end_of_stream(mut self) -> Result<(), Error> {
        // Send the last elements in the queue.
        if !self.buffer.is_empty() {
            self.flush_buffer().await?;
        }

        // Send an empty batch to signal the end of the stream.
        self.flush_buffer().await?;

        Ok(())
    }

    async fn flush_buffer(&mut self) -> Result<(), Error> {
        // Read and reset the buffer.
        let buffer = std::mem::replace(&mut self.buffer, Vec::new());
        self.buffer_size = EMPTY_BUFFER_SIZE;

        // Send it.
        let fut = self.dest.batch(&buffer);
        Ok(fut.await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async as fasync;
    use maplit::hashmap;
    use std::collections::HashMap;
    use test_case::test_case;

    use crate::snapshot::Snapshot;

    // Generate an allocation hash map with a huge number of entries, to test that pagination splits
    // them properly.
    fn generate_one_million_allocations_hashmap() -> HashMap<u64, u64> {
        let mut result = HashMap::new();
        let mut addr = 0;
        for size in 0..1000000 {
            result.insert(addr, size);
            addr += size;
        }
        result
    }

    const FAKE_TIMESTAMP: i64 = 12345678;
    const FAKE_THREAD_KOID: u64 = 8989;
    const FAKE_THREAD_NAME: &str = "fake-thread-name";
    const FAKE_THREAD_KEY: u64 = 1212;
    const FAKE_STACK_TRACE_ADDRESSES: [u64; 3] = [11111, 22222, 33333];
    const FAKE_STACK_TRACE_KEY: u64 = 1234;
    const FAKE_REGION_ADDRESS: u64 = 8192;
    const FAKE_REGION_SIZE: u64 = 28672;
    const FAKE_REGION_FILE_OFFSET: u64 = 4096;
    const FAKE_REGION_BUILD_ID: &[u8] = &[0xee; 20];

    #[test_case(hashmap! {} ; "empty")]
    #[test_case(hashmap! { 1234 => 5678 } ; "only one")]
    #[test_case(generate_one_million_allocations_hashmap() ; "one million")]
    #[fasync::run_singlethreaded(test)]
    async fn test_streamer(allocations: HashMap<u64, u64>) {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Transmit a snapshot with the given `allocations`, all referencing the same thread info
        // and stack trace, with a single executable region.
        let mut streamer = Streamer::new(receiver_proxy)
            .push_element(fheapdump_client::SnapshotElement::ThreadInfo(
                fheapdump_client::ThreadInfo {
                    thread_info_key: Some(FAKE_THREAD_KEY),
                    koid: Some(FAKE_THREAD_KOID),
                    name: Some(FAKE_THREAD_NAME.to_string()),
                    ..Default::default()
                },
            ))
            .await
            .unwrap()
            .push_element(fheapdump_client::SnapshotElement::StackTrace(
                fheapdump_client::StackTrace {
                    stack_trace_key: Some(FAKE_STACK_TRACE_KEY),
                    program_addresses: Some(FAKE_STACK_TRACE_ADDRESSES.to_vec()),
                    ..Default::default()
                },
            ))
            .await
            .unwrap()
            .push_element(fheapdump_client::SnapshotElement::ExecutableRegion(
                fheapdump_client::ExecutableRegion {
                    address: Some(FAKE_REGION_ADDRESS),
                    size: Some(FAKE_REGION_SIZE),
                    file_offset: Some(FAKE_REGION_FILE_OFFSET),
                    build_id: Some(fheapdump_client::BuildId {
                        value: FAKE_REGION_BUILD_ID.to_vec(),
                    }),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
        for (address, size) in &allocations {
            streamer = streamer
                .push_element(fheapdump_client::SnapshotElement::Allocation(
                    fheapdump_client::Allocation {
                        address: Some(*address),
                        size: Some(*size),
                        thread_info_key: Some(FAKE_THREAD_KEY),
                        stack_trace_key: Some(FAKE_STACK_TRACE_KEY),
                        timestamp: Some(FAKE_TIMESTAMP),
                        ..Default::default()
                    },
                ))
                .await
                .unwrap();
        }
        streamer.end_of_stream().await.unwrap();

        // Receive the snapshot we just transmitted and verify that the allocations and the
        // executable region we received match those that were sent.
        let mut received_snapshot = receive_worker.await.unwrap();
        for (address, size) in &allocations {
            let allocation = received_snapshot.allocations.remove(address).unwrap();
            assert_eq!(allocation.size, *size);
            assert_eq!(allocation.thread_info.koid, FAKE_THREAD_KOID);
            assert_eq!(allocation.thread_info.name, FAKE_THREAD_NAME);
            assert_eq!(allocation.stack_trace.program_addresses, FAKE_STACK_TRACE_ADDRESSES);
            assert_eq!(allocation.timestamp, FAKE_TIMESTAMP);
        }
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
        let region = received_snapshot.executable_regions.remove(&FAKE_REGION_ADDRESS).unwrap();
        assert_eq!(region.size, FAKE_REGION_SIZE);
        assert_eq!(region.file_offset, FAKE_REGION_FILE_OFFSET);
        assert_eq!(region.build_id, FAKE_REGION_BUILD_ID);
        assert!(received_snapshot.executable_regions.is_empty(), "all entries have been removed");
    }
}
