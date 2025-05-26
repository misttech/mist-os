// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use block_client::{BlockClient as _, BufferSlice, MutableBufferSlice};
use block_protocol::{BlockFifoCommand, BlockFifoRequest, BlockFifoResponse};
use fake_block_server::{FakeServer, FakeServerOptions, Observer};
use fidl_fuchsia_hardware_block_driver::{BlockIoFlag, BlockOpcode};
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use test_case::test_case;
use zx::{AsHandleRef, HandleBased as _};
use {
    fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume,
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
};

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

enum Ramdisk {
    V1,
    V2,
}

#[test_case(Ramdisk::V1; "v1")]
#[test_case(Ramdisk::V2; "v2")]
#[fuchsia::test]
async fn test_group_with_close(version: Ramdisk) {
    let (proxy, server) = fidl::endpoints::create_proxy::<fvolume::VolumeMarker>();

    let builder = ramdevice_client::RamdiskClientBuilder::new(BLOCK_SIZE as u64, NUM_BLOCKS);
    let ramdisk = match version {
        Ramdisk::V1 => builder,
        Ramdisk::V2 => builder.use_v2(),
    }
    .build()
    .await
    .expect("Failed to create ramdisk");
    ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");

    let (session_proxy, server) = fidl::endpoints::create_proxy();

    proxy.open_session(server).unwrap();

    let vmo1 = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
    let vmo_id1 = session_proxy
        .attach_vmo(vmo1.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
        .await
        .unwrap()
        .unwrap();

    let vmo2 = zx::Vmo::create(zx::system_get_page_size() as u64).unwrap();
    let vmo_id2 = session_proxy
        .attach_vmo(vmo2.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap())
        .await
        .unwrap()
        .unwrap();

    let mut fifo = fasync::Fifo::<_, BlockFifoRequest>::from_fifo(
        session_proxy.get_fifo().await.unwrap().unwrap(),
    );
    let (mut reader, mut writer) = fifo.async_io();

    writer
        .write_entries(&[
            BlockFifoRequest {
                command: BlockFifoCommand {
                    opcode: BlockOpcode::Read.into_primitive(),
                    flags: BlockIoFlag::GROUP_ITEM.bits(),
                    ..Default::default()
                },
                group: 7,
                vmoid: vmo_id1.id,
                length: 1,
                ..Default::default()
            },
            BlockFifoRequest {
                command: BlockFifoCommand {
                    opcode: BlockOpcode::Read.into_primitive(),
                    flags: (BlockIoFlag::GROUP_ITEM | BlockIoFlag::GROUP_LAST).bits(),
                    ..Default::default()
                },
                group: 7,
                reqid: 10,
                vmoid: vmo_id2.id,
                length: 1,
                ..Default::default()
            },
            BlockFifoRequest {
                command: BlockFifoCommand {
                    opcode: BlockOpcode::CloseVmo.into_primitive(),
                    ..Default::default()
                },
                reqid: 11,
                vmoid: vmo_id1.id,
                ..Default::default()
            },
            BlockFifoRequest {
                command: BlockFifoCommand {
                    opcode: BlockOpcode::CloseVmo.into_primitive(),
                    ..Default::default()
                },
                reqid: 12,
                vmoid: vmo_id2.id,
                ..Default::default()
            },
        ])
        .await
        .unwrap();

    let mut responses = 0;
    for _ in 0..3 {
        let mut response = BlockFifoResponse::default();
        reader.read_entries(&mut response).await.unwrap();
        assert_eq!(response.status, zx::sys::ZX_OK);
        assert!(response.reqid >= 10 && response.reqid < 13);
        let bit = 1 << (response.reqid - 10);
        assert_eq!(responses & bit, 0);
        responses |= bit;
    }

    ramdisk.destroy_and_wait_for_removal().await.unwrap();
}

#[fuchsia::test]
async fn test_gpt_on_ramdisk() {
    let ramdisk = ramdevice_client::RamdiskClientBuilder::new(BLOCK_SIZE as u64, NUM_BLOCKS)
        .use_v2()
        .max_transfer_blocks(MAX_TRANSFER_BLOCKS)
        .build()
        .await
        .expect("Failed to create ramdisk");

    const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
    const PART1_INSTANCE_GUID: [u8; 16] = [2u8; 16];
    const PART1_NAME: &str = "part";
    const PART2_INSTANCE_GUID: [u8; 16] = [3u8; 16];
    const PART2_NAME: &str = "part2";
    {
        let (proxy, server) = fidl::endpoints::create_proxy::<fvolume::VolumeMarker>();
        ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");
        let client = Arc::new(block_client::RemoteBlockClient::new(proxy).await.unwrap());
        gpt::Gpt::format(
            client,
            vec![
                gpt::PartitionInfo {
                    label: PART1_NAME.to_string(),
                    type_guid: gpt::Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: gpt::Guid::from_bytes(PART1_INSTANCE_GUID),
                    start_block: 3000,
                    num_blocks: 200,
                    flags: 0xabcd,
                },
                gpt::PartitionInfo {
                    label: PART2_NAME.to_string(),
                    type_guid: gpt::Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: gpt::Guid::from_bytes(PART2_INSTANCE_GUID),
                    start_block: 3200,
                    num_blocks: 400,
                    flags: 0xabcd,
                },
            ],
        )
        .await
        .unwrap();
    }

    let (proxy, server) = fidl::endpoints::create_proxy::<fblock::BlockMarker>();
    ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");

    let partitions_dir = vfs::directory::immutable::simple();
    let partitions_dir_clone = partitions_dir.clone();
    let runner = gpt_component::gpt::GptManager::new(proxy, partitions_dir_clone)
        .await
        .expect("load should succeed");

    let part1_dir = vfs::serve_directory(
        partitions_dir.clone(),
        vfs::path::Path::validate_and_split("part-000").unwrap(),
        fio::PERM_READABLE,
    );
    let part1_block = fuchsia_component_client::connect_to_named_protocol_at_dir_root::<
        fvolume::VolumeMarker,
    >(&part1_dir, "volume")
    .expect("Failed to open Volume service");

    let part2_dir = vfs::serve_directory(
        partitions_dir.clone(),
        vfs::path::Path::validate_and_split("part-001").unwrap(),
        fio::PERM_READABLE,
    );
    let part2_block = fuchsia_component_client::connect_to_named_protocol_at_dir_root::<
        fvolume::VolumeMarker,
    >(&part2_dir, "volume")
    .expect("Failed to open Volume service");

    {
        let client1 = block_client::RemoteBlockClient::new(part1_block).await.unwrap();
        let client2 = block_client::RemoteBlockClient::new(part2_block).await.unwrap();
        const BS: usize = BLOCK_SIZE as usize;
        const LEN: u64 = 50 * BS as u64;
        let vmo = zx::Vmo::create(LEN).unwrap();
        let vmoid1 = client1.attach_vmo(&vmo).await.expect("attach_vmo failed");
        let vmoid2 = client2.attach_vmo(&vmo).await.expect("attach_vmo failed");

        vmo.write(&[0x11u8; LEN as usize], 0).unwrap();
        client1
            .write_at(BufferSlice::new_with_vmo_id(&vmoid1, 0, LEN), BS as u64 * 150)
            .await
            .expect("write failed");
        vmo.write(&[0u8; LEN as usize], 0).unwrap();
        client1
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmoid1, 0, LEN), BS as u64 * 150)
            .await
            .expect("read failed");
        assert_eq!(&vmo.read_to_vec(0, LEN).unwrap()[..], &[0x11u8; LEN as usize]);

        vmo.write(&[0x22u8; LEN as usize], 0).unwrap();
        client2
            .write_at(BufferSlice::new_with_vmo_id(&vmoid2, 0, BS as u64), 0)
            .await
            .expect("write failed");
        vmo.write(&[0u8; LEN as usize], 0).unwrap();
        client2
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmoid2, 0, BS as u64), 0)
            .await
            .expect("read failed");
        assert_eq!(&vmo.read_to_vec(0, BS as u64).unwrap()[..], &[0x22u8; BS]);

        // Write past end
        vmo.write(&[0x33u8; LEN as usize], 0).unwrap();
        client1
            .write_at(BufferSlice::new_with_vmo_id(&vmoid1, 0, 2 * BS as u64), BS as u64 * 199)
            .await
            .expect_err("write past end should fail");
        // Other partition should be unchanged
        vmo.write(&[0u8; LEN as usize], 0).unwrap();
        client2
            .read_at(MutableBufferSlice::new_with_vmo_id(&vmoid2, 0, BS as u64), 0)
            .await
            .expect("read failed");
        assert_eq!(&vmo.read_to_vec(0, BS as u64).unwrap()[..], &[0x22u8; BS]);

        client1.detach_vmo(vmoid1).await.unwrap();
        client2.detach_vmo(vmoid2).await.unwrap();
    }

    runner.shutdown().await;
    ramdisk.destroy_and_wait_for_removal().await.unwrap();
}

// The test uses a separate ramdisk for an underlying block device, and runs a local GPT instance on
// top of that.  Then, the test uses synchronous interfaces to send a request to the session.  If
// passthrough is enabled, the GPT component will be completely bypassed by requests, and this will
// work.  If passthrough is not enabled, then the test will deadlock (as the sync call will block
// forever, because the test is single-threaded).
// Note that the test must be executed on a single thread to be useful.
#[fuchsia::test]
async fn test_gpt_passthrough_is_enabled() {
    let ramdisk = ramdevice_client::RamdiskClientBuilder::new(BLOCK_SIZE as u64, NUM_BLOCKS)
        .use_v2()
        .max_transfer_blocks(MAX_TRANSFER_BLOCKS)
        .build()
        .await
        .expect("Failed to create ramdisk");

    const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
    const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
    const PART_NAME: &str = "part";
    {
        let (proxy, server) = fidl::endpoints::create_proxy::<fvolume::VolumeMarker>();
        ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");
        let client = Arc::new(block_client::RemoteBlockClient::new(proxy).await.unwrap());
        gpt::Gpt::format(
            client,
            vec![gpt::PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: gpt::Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: gpt::Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 2,
                flags: 0xabcd,
            }],
        )
        .await
        .unwrap();
    }

    let (proxy, server) = fidl::endpoints::create_proxy::<fblock::BlockMarker>();
    ramdisk.connect(server.into_channel().into()).expect("Failed to connect to ramdisk");

    let partitions_dir = vfs::directory::immutable::simple();
    let partitions_dir_clone = partitions_dir.clone();
    let runner = gpt_component::gpt::GptManager::new(proxy, partitions_dir_clone)
        .await
        .expect("load should succeed");

    let part_dir = vfs::serve_directory(
        partitions_dir.clone(),
        vfs::path::Path::validate_and_split("part-000").unwrap(),
        fio::PERM_READABLE,
    );
    let part_block = fuchsia_component_client::connect_to_named_protocol_at_dir_root::<
        fvolume::VolumeMarker,
    >(&part_dir, "volume")
    .expect("Failed to open Volume service");

    {
        let (session_proxy, server) = fidl::endpoints::create_proxy();

        part_block.open_session(server).unwrap();

        let fifo: zx::Fifo<BlockFifoResponse, BlockFifoRequest> =
            session_proxy.get_fifo().await.unwrap().unwrap().into();

        fifo.write(&[BlockFifoRequest {
            command: BlockFifoCommand {
                opcode: BlockOpcode::Flush.into_primitive(),
                ..Default::default()
            },
            ..Default::default()
        }])
        .unwrap();
        loop {
            match fifo.read_one() {
                Ok(response) => {
                    zx::Status::ok(response.status).expect("Flush failed");
                    break;
                }
                Err(zx::Status::SHOULD_WAIT) => {
                    fifo.wait_handle(zx::Signals::FIFO_READABLE, zx::MonotonicInstant::INFINITE)
                        .unwrap();
                }
                err => {
                    err.unwrap();
                }
            }
        }
    }

    runner.shutdown().await;
    ramdisk.destroy_and_wait_for_removal().await.unwrap();
}
