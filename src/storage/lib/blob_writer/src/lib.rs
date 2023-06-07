// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::errors::{CreateError, WriteError},
    fidl_fuchsia_fxfs::BlobWriterProxy,
    fuchsia_zircon as zx,
    futures::{
        future::{BoxFuture, FutureExt},
        stream::{FuturesOrdered, StreamExt},
    },
};

mod errors;

/// BlobWriter is a wrapper around the fuchsia.fxfs.BlobWriter fidl protocol. Clients will use this
/// library to write blobs to disk.
pub struct BlobWriter {
    blob_writer_proxy: BlobWriterProxy,
    vmo: zx::Vmo,
    // Ordered queue of BytesReady requests. There are at most 2 outstanding requests on the
    // queue at any point in time. Each BytesReady request takes up at most half the ring
    // buffer (N).
    //
    // Our goal is to be constantly moving bytes out of the network and into storage without ever
    // having to wait for a fidl roundtrip. Maintaining 2 outstanding requests on the queue allows
    // us to pipeline requests, so that the server can respond to one request while the client is
    // creating another. Limiting the size of any particular request to N/2 allows each of the
    // two requests on the queue to be as big as they possibly can, which is particularly important
    // when storage is the limiting factor. Namely, we want to avoid situations where the server
    // has completed a small request and has to wait on a fidl roundtrip (i.e. has to wait for the
    // network to receive the response, create a new request, and send the request back).
    outstanding_writes: FuturesOrdered<BoxFuture<'static, Result<Result<u64, i32>, fidl::Error>>>,
    // Total number of bytes that have been written to the ring buffer.
    write_index: u64,
    // Number of available bytes in the ring buffer.
    available: u64,
    // Size of the blob being written.
    data_len: u64,
}

impl BlobWriter {
    /// Creates a `BlobWriter`.  Exactly `size` bytes are expected to be written into the writer.
    pub async fn create(
        blob_writer_proxy: BlobWriterProxy,
        size: u64,
    ) -> Result<Self, CreateError> {
        let vmo = blob_writer_proxy
            .get_vmo(size)
            .await
            .map_err(CreateError::Fidl)?
            .map_err(zx::Status::from_raw)
            .map_err(CreateError::GetVmo)?;
        let vmo_size = vmo.get_size().map_err(CreateError::GetSize)?;
        Ok(BlobWriter {
            blob_writer_proxy,
            vmo,
            outstanding_writes: FuturesOrdered::new(),
            write_index: 0,
            available: vmo_size,
            data_len: size,
        })
    }

    /// Writes an additional set of `bytes` to the server. The blob will only be readable after the
    /// final `write` has been completed, which happens when the number of bytes sent to the server
    /// is equal to the `size` initially passed into `create`. Returns an error if the length of
    /// `bytes` exceeds the remaining available space in the blob, calculated as per `size`.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        let mut bytes_left_to_write = bytes.len() as u64;
        if self.write_index + bytes_left_to_write > self.data_len {
            return Err(WriteError::EndOfBlob);
        }
        let mut bytes_written = 0;
        let vmo_size = self.vmo.get_size().map_err(WriteError::GetSize)?;
        while bytes_left_to_write > 0 {
            // If there is no space available on the ring buffer or the queue already has 2
            // requests on it, wait for a BytesReady request to complete and take it off the queue.
            if self.available == 0 || self.outstanding_writes.len() == 2 {
                let bytes_that_were_written = self
                    .outstanding_writes
                    .next()
                    .await
                    .ok_or_else(|| WriteError::QueueEnded)?
                    .map_err(WriteError::Fidl)?
                    .map_err(zx::Status::from_raw)
                    .map_err(WriteError::BytesReady)?;
                self.available += bytes_that_were_written;
            }
            let bytes_available = std::cmp::min(vmo_size / 2, self.available);
            let bytes_to_write = std::cmp::min(bytes_available, bytes_left_to_write);
            let curr_index_vmo = self.write_index % vmo_size;

            // If the current write requires wrapping around the ring buffer..
            if curr_index_vmo + bytes_to_write > vmo_size {
                let left_in_vmo = vmo_size - curr_index_vmo;
                self.vmo
                    .write(
                        &bytes[bytes_written..bytes_written + left_in_vmo as usize],
                        curr_index_vmo,
                    )
                    .map_err(WriteError::VmoWrite)?;
                bytes_written += left_in_vmo as usize;
                let left_to_write = bytes_to_write - left_in_vmo;
                self.vmo
                    .write(&bytes[bytes_written..bytes_written + left_to_write as usize], 0)
                    .map_err(WriteError::VmoWrite)?;
                bytes_written += left_to_write as usize;
            }
            // Else is the current write can be written without wrapping..
            else {
                self.vmo
                    .write(
                        &bytes[bytes_written..bytes_written + bytes_to_write as usize],
                        curr_index_vmo,
                    )
                    .map_err(WriteError::VmoWrite)?;
                bytes_written += bytes_to_write as usize;
            }
            // Create a BytesReady request and push it onto the queue. Update all the relevant
            // counters.
            let write_fut = self.blob_writer_proxy.bytes_ready(bytes_to_write);
            self.outstanding_writes.push(
                async move { write_fut.await.map(|res| res.map(|_| bytes_to_write)) }.boxed(),
            );
            self.available -= bytes_to_write;
            self.write_index += bytes_to_write;
            bytes_left_to_write -= bytes_to_write;
        }

        // If finished writing the blob, empty out the outstanding_writes queue.
        if self.write_index == self.data_len {
            while let Some(result) = self.outstanding_writes.next().await {
                if let Err(e) = result.map_err(WriteError::Fidl)? {
                    return Err(WriteError::BytesReady(zx::Status::from_raw(e)));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_fxfs::{BlobWriterMarker, BlobWriterRequest},
        fuchsia_zircon::HandleBased,
        futures::{future::BoxFuture, pin_mut, select, FutureExt},
        rand::{thread_rng, Rng},
        std::sync::{Arc, Mutex},
    };

    async fn check_blob_writer(
        write_fun: impl FnOnce(BlobWriterProxy) -> BoxFuture<'static, ()>,
        data: &[u8],
        writes: Vec<(usize, usize)>,
    ) {
        let (proxy, mut stream) = create_proxy_and_stream::<BlobWriterMarker>().unwrap();
        let count = Arc::new(Mutex::new(0));
        let count_clone = count.clone();
        let expected_count = writes.len();
        let mut check_vmo = None;
        let mock_server = async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(BlobWriterRequest::GetVmo { responder, .. }) => {
                        let vmo = zx::Vmo::create(4096 * 64).expect("failed to create vmo");
                        let vmo_dup = vmo
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("failed to duplicate VMO");
                        check_vmo = Some(vmo);
                        responder.send(Ok(vmo_dup)).unwrap();
                    }
                    Ok(BlobWriterRequest::BytesReady { responder, bytes_written, .. }) => {
                        let vmo = check_vmo.as_ref().unwrap();
                        let mut count_locked = count.lock().unwrap();
                        let mut buf = vec![0; bytes_written as usize];
                        let data_range = writes[*count_locked];
                        let vmo_size = 4096 * 64;
                        let vmo_offset = data_range.0 % vmo_size;
                        if vmo_offset + bytes_written as usize > vmo_size {
                            let split = vmo_size - vmo_offset;
                            vmo.read(&mut buf[0..split], vmo_offset as u64).unwrap();
                            vmo.read(&mut buf[split..], 0).unwrap();
                        } else {
                            vmo.read(&mut buf, vmo_offset as u64).unwrap();
                        }
                        assert_eq!(bytes_written, (data_range.1 - data_range.0) as u64);
                        assert_eq!(&data[data_range.0..data_range.1], buf);
                        *count_locked += 1;
                        responder.send(Ok(())).unwrap();
                    }
                    _ => {
                        unreachable!()
                    }
                }
            }
        }
        .fuse();

        pin_mut!(mock_server);

        select! {
            _ = mock_server => unreachable!(),
            _ = write_fun(proxy).fuse() => {
                assert_eq!(*count_clone.lock().unwrap(), expected_count);
            }
        }
    }

    #[fuchsia::test]
    async fn invalid_write_past_end_of_blob_test() {
        let mut data = [1; 32768];
        thread_rng().fill(&mut data[..]);

        let list_of_writes = vec![(0..8192), (8192..24576), (24576..32768)];

        let write_fun = |proxy: BlobWriterProxy| {
            async move {
                let mut blob_writer = BlobWriter::create(proxy, data.len() as u64)
                    .await
                    .expect("failed to create BlobWriter");
                for write in list_of_writes {
                    blob_writer.write(&data[write]).await.unwrap();
                }
                let invalid_write = [1; 4096];
                let error = blob_writer.write(&invalid_write).await.expect_err(
                    "invalid write past the expected end of blob unexpectedly succeeded",
                );
                match error {
                    WriteError::EndOfBlob => (),
                    _ => panic!("expected EndOfBlob error"),
                }
            }
            .boxed()
        };

        check_blob_writer(write_fun, &data, vec![(0, 8192), (8192, 24576), (24576, 32768)]).await;
    }

    #[fuchsia::test]
    async fn small_write_one_slice_test() {
        let mut data = [1; 32768];
        thread_rng().fill(&mut data[..]);

        let list_of_writes = vec![(0..8192), (8192..24576), (24576..32768)];

        let write_fun = |proxy: BlobWriterProxy| {
            async move {
                let mut blob_writer = BlobWriter::create(proxy, data.len() as u64)
                    .await
                    .expect("failed to create BlobWriter");
                for write in list_of_writes {
                    blob_writer.write(&data[write]).await.unwrap();
                }
            }
            .boxed()
        };

        check_blob_writer(write_fun, &data, vec![(0, 8192), (8192, 24576), (24576, 32768)]).await;
    }

    #[fuchsia::test]
    async fn small_write_multiple_slice_no_wrap_test() {
        let mut data = [1; 196608];
        thread_rng().fill(&mut data[..]);

        let list_of_writes =
            vec![(0..8192), (8192..24576), (24576..32768), (32768..98304), (98304..196608)];
        let write_fun = |proxy: BlobWriterProxy| {
            async move {
                let mut blob_writer = BlobWriter::create(proxy, data.len() as u64)
                    .await
                    .expect("failed to create BlobWriter");
                for write in list_of_writes {
                    blob_writer.write(&data[write]).await.unwrap();
                }
            }
            .boxed()
        };

        check_blob_writer(
            write_fun,
            &data,
            vec![(0, 8192), (8192, 24576), (24576, 32768), (32768, 98304), (98304, 196608)],
        )
        .await;
    }

    #[fuchsia::test]
    async fn large_write_one_wrap_test() {
        let mut data = [1; 303104];
        thread_rng().fill(&mut data[..]);

        let list_of_writes = vec![
            (0..8192),
            (8192..24576),
            (24576..32768),
            (32768..98304),
            (98304..196608),
            (196608..204800),
            (204800..237568),
            (237568..303104),
        ];
        let write_fun = |proxy: BlobWriterProxy| {
            async move {
                let mut blob_writer = BlobWriter::create(proxy, data.len() as u64)
                    .await
                    .expect("failed to create BlobWriter");
                for write in list_of_writes {
                    blob_writer.write(&data[write]).await.unwrap();
                }
            }
            .boxed()
        };

        check_blob_writer(
            write_fun,
            &data,
            vec![
                (0, 8192),
                (8192, 24576),
                (24576, 32768),
                (32768, 98304),
                (98304, 196608),
                (196608, 204800),
                (204800, 237568),
                (237568, 303104),
            ],
        )
        .await;
    }

    #[fuchsia::test]
    async fn large_write_multiple_wraps_test() {
        let mut data = [1; 499712];
        thread_rng().fill(&mut data[..]);

        let list_of_writes = vec![
            (0..8192),
            (8192..24576),
            (24576..32768),
            (32768..98304),
            (98304..196608),
            (196608..204800),
            (204800..237568),
            (237568..303104),
            (303104..401408),
            (401408..499712),
        ];
        let write_fun = |proxy: BlobWriterProxy| {
            async move {
                let mut blob_writer = BlobWriter::create(proxy, data.len() as u64)
                    .await
                    .expect("failed to create BlobWriter");
                for write in list_of_writes {
                    blob_writer.write(&data[write]).await.unwrap();
                }
            }
            .boxed()
        };

        check_blob_writer(
            write_fun,
            &data,
            vec![
                (0, 8192),
                (8192, 24576),
                (24576, 32768),
                (32768, 98304),
                (98304, 196608),
                (196608, 204800),
                (204800, 237568),
                (237568, 303104),
                (303104, 401408),
                (401408, 499712),
            ],
        )
        .await;
    }
}
