// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use block_protocol::{BlockFifoCommand, BlockFifoRequest, BlockFifoResponse};
use fake_block_server::{FakeServer, FakeServerOptions, Observer};
use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use zx::HandleBased as _;
use {fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_async as fasync};

// Make the block device big enough so that we can have a request which creates more than
// block_server::MAX_REQUESTS.
const MAX_TRANSFER_BLOCKS: u32 = 10;
const NUM_BLOCKS: u64 = 10_000;
const BLOCK_SIZE: u32 = 512;
// The FIFO server can handle up to 64 simultaneous requests, so do one more than that to
// exercise handling really big requests.
const REQ_BLOCKS: usize = 65 * MAX_TRANSFER_BLOCKS as usize;
const REQ_SIZE: u64 = REQ_BLOCKS as u64 * BLOCK_SIZE as u64;

async fn test_request_splitting_client_fn(
    proxy: fvolume::VolumeProxy,
    last_trim_length_fn: Option<Box<dyn Fn() -> u32>>,
) {
    let bs = BLOCK_SIZE as usize;

    let (session_proxy, server) = fidl::endpoints::create_proxy();

    proxy.open_session(server).unwrap();

    let vmo = zx::Vmo::create(REQ_SIZE).unwrap();
    let vmo_id = session_proxy
        .attach_vmo(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
        .await
        .unwrap()
        .unwrap();

    let mut fifo = fasync::Fifo::from_fifo(session_proxy.get_fifo().await.unwrap().unwrap());
    let (mut reader, mut writer) = fifo.async_io();

    // Fill in a predictable pattern so we can detect reading/writing to the correct blocks.
    for i in 0..REQ_BLOCKS {
        vmo.write(&[i as u8], (i * bs) as u64).expect("vmo write failed");
    }

    writer
        .write_entries(&BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Write.into_primitive(),
                ..Default::default()
            },
            vmoid: vmo_id.id,
            dev_offset: 10,
            length: REQ_BLOCKS as u32,
            vmo_offset: 0,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut response = BlockFifoResponse::default();
    reader.read_entries(&mut response).await.unwrap();
    assert_eq!(response.status, zx::sys::ZX_OK);

    for i in 0..REQ_BLOCKS {
        vmo.write(&[0 as u8], (i * bs) as u64).expect("vmo write failed");
    }

    writer
        .write_entries(&BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Read.into_primitive(),
                ..Default::default()
            },
            vmoid: vmo_id.id,
            dev_offset: 10,
            length: REQ_BLOCKS as u32,
            vmo_offset: 0,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut response = BlockFifoResponse::default();
    reader.read_entries(&mut response).await.unwrap();
    assert_eq!(response.status, zx::sys::ZX_OK);

    for i in 0..REQ_BLOCKS {
        let mut buf = [0u8];
        vmo.read(&mut buf, (i * bs) as u64).expect("vmo read failed");
        assert_eq!(buf[0], i as u8);
    }

    if let Some(last_trim_length_fn) = last_trim_length_fn.as_ref() {
        assert_eq!(0, last_trim_length_fn());
        writer
            .write_entries(&BlockFifoRequest {
                command: BlockFifoCommand {
                    opcode: BlockOpcode::Trim.into_primitive(),
                    ..Default::default()
                },
                group: 1,
                dev_offset: 0,
                length: 400,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut response = BlockFifoResponse::default();
        reader.read_entries(&mut response).await.unwrap();
        assert_eq!(response.status, zx::sys::ZX_OK);
        assert_eq!(400, last_trim_length_fn());
    }

    // OK, now put several big requests in a group and make sure that works.
    writer
        .write_entries(&BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Write.into_primitive(),
                flags: BlockIoFlag::GROUP_ITEM.bits(),
                ..Default::default()
            },
            group: 1,
            vmoid: vmo_id.id,
            dev_offset: 0,
            length: 1,
            vmo_offset: 0,
            ..Default::default()
        })
        .await
        .unwrap();
    writer
        .write_entries(&BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Read.into_primitive(),
                flags: BlockIoFlag::GROUP_ITEM.bits(),
                ..Default::default()
            },
            group: 1,
            vmoid: vmo_id.id,
            dev_offset: 0,
            length: (REQ_BLOCKS / 2) as u32,
            vmo_offset: 0,
            ..Default::default()
        })
        .await
        .unwrap();
    writer
        .write_entries(&BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Read.into_primitive(),
                flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                ..Default::default()
            },
            group: 1,
            vmoid: vmo_id.id,
            dev_offset: REQ_BLOCKS as u64 / 2,
            length: REQ_BLOCKS as u32 / 2,
            vmo_offset: REQ_BLOCKS as u64 / 2,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut response = BlockFifoResponse::default();
    reader.read_entries(&mut response).await.unwrap();
    assert_eq!(response.status, zx::sys::ZX_OK);
    assert_eq!(response.group, 1);

    std::mem::drop(proxy);
}

#[fuchsia::test]
async fn test_request_splitting_rust_server() {
    let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();

    // Records the length of the last trim command.
    struct TrimObserver(Arc<AtomicU32>);
    impl Observer for TrimObserver {
        fn trim(&self, _device_block_offset: u64, block_count: u32) {
            self.0.store(block_count, Ordering::Relaxed)
        }
    }

    let last_trim_length = Arc::new(AtomicU32::new(0));
    let last_trim_length_clone = last_trim_length.clone();

    let server = async {
        let block_server = FakeServer::from(FakeServerOptions {
            block_count: Some(NUM_BLOCKS),
            block_size: BLOCK_SIZE,
            max_transfer_blocks: NonZero::new(MAX_TRANSFER_BLOCKS),
            observer: Some(Box::new(TrimObserver(last_trim_length_clone))),
            ..Default::default()
        });
        block_server.serve(stream).await.unwrap();
    };

    let client = test_request_splitting_client_fn(
        proxy,
        Some(Box::new(move || last_trim_length.load(Ordering::Relaxed))),
    );

    futures::join!(server, client);
}

#[fuchsia::test]
async fn test_request_splitting_cpp_server() {
    let (proxy, server) = fidl::endpoints::create_proxy::<fvolume::VolumeMarker>();

    let ramdisk = ramdevice_client::RamdiskClientBuilder::new(BLOCK_SIZE as u64, NUM_BLOCKS)
        .use_v2()
        .max_transfer_blocks(MAX_TRANSFER_BLOCKS)
        .build()
        .await
        .expect("Failed to create ramdisk");
    ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");

    test_request_splitting_client_fn(proxy, None).await;

    ramdisk.destroy_and_wait_for_removal().await.unwrap();
}
