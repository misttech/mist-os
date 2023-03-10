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
        let mut buffer = std::mem::replace(&mut self.buffer, Vec::new());
        self.buffer_size = EMPTY_BUFFER_SIZE;

        // Send it.
        let fut = self.dest.batch(&mut buffer.iter_mut());
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

    #[test_case(hashmap! {} ; "empty")]
    #[test_case(hashmap! { 1234 => 5678 } ; "only one")]
    #[test_case(generate_one_million_allocations_hashmap() ; "one million")]
    #[fasync::run_singlethreaded(test)]
    async fn test_streamer(allocations: HashMap<u64, u64>) {
        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
        let receive_worker = fasync::Task::local(Snapshot::receive_from(receiver_stream));

        // Transmit a snapshot with the given `allocations`.
        let mut streamer = Streamer::new(receiver_proxy);
        for (address, size) in &allocations {
            streamer = streamer
                .push_element(fheapdump_client::SnapshotElement::Allocation(
                    fheapdump_client::Allocation {
                        address: Some(*address),
                        size: Some(*size),
                        ..fheapdump_client::Allocation::EMPTY
                    },
                ))
                .await
                .unwrap();
        }
        streamer.end_of_stream().await.unwrap();

        // Receive the snapshot we just transmitted and verify that the allocations we received
        // match those that were sent.
        let mut received_snapshot = receive_worker.await.unwrap();
        for (address, size) in &allocations {
            let allocation = received_snapshot.allocations.remove(address).unwrap();
            assert_eq!(allocation.size, *size);
        }
        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
    }
}
