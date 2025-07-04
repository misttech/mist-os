// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/core/block-device.h"

#include <fidl/fuchsia.hardware.block.partition/cpp/natural_types.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <lib/fidl/cpp/wire/string_view.h>
#include <lib/operation/block.h>
#include <lib/zbi-format/partition.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <threads.h>
#include <zircon/threads.h>

#include "src/devices/block/drivers/core/server.h"

zx_status_t BlockDevice::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  switch (proto_id) {
    case ZX_PROTOCOL_BLOCK: {
      self_protocol_.GetProto(static_cast<block_protocol_t*>(out_protocol));
      return ZX_OK;
    }
    case ZX_PROTOCOL_BLOCK_PARTITION: {
      if (!parent_partition_protocol_.is_valid()) {
        return ZX_ERR_NOT_SUPPORTED;
      }
      parent_partition_protocol_.GetProto(static_cast<block_partition_protocol_t*>(out_protocol));
      return ZX_OK;
    }
    case ZX_PROTOCOL_BLOCK_VOLUME: {
      if (!parent_volume_protocol_.is_valid()) {
        return ZX_ERR_NOT_SUPPORTED;
      }
      parent_volume_protocol_.GetProto(static_cast<block_volume_protocol_t*>(out_protocol));
      return ZX_OK;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

// Define the maximum I/O possible for the midlayer; this is arbitrarily
// set to the size of RIO's max payload.
//
// If a smaller value of "max_transfer_size" is defined, that will
// be used instead.
constexpr uint32_t kMaxMidlayerIO = 8192;

zx_status_t BlockDevice::DoIo(zx::vmo& vmo, size_t buf_len, zx_off_t off, zx_off_t vmo_off,
                              bool write) {
  std::lock_guard<std::mutex> lock(io_lock_);
  const size_t block_size = info_.block_size;
  const size_t max_xfer = std::min(info_.max_transfer_size, kMaxMidlayerIO);

  if (buf_len == 0) {
    return ZX_OK;
  }
  if ((buf_len % block_size) || (off % block_size)) {
    return ZX_ERR_INVALID_ARGS;
  }

  uint64_t sub_txn_offset = 0;
  while (sub_txn_offset < buf_len) {
    size_t sub_txn_length = std::min(buf_len - sub_txn_offset, max_xfer);

    block_op_t* op = reinterpret_cast<block_op_t*>(io_op_.get());
    const uint8_t opcode = write ? BLOCK_OPCODE_WRITE : BLOCK_OPCODE_READ;
    op->command = {.opcode = opcode, .flags = 0};
    ZX_DEBUG_ASSERT(sub_txn_length / block_size < std::numeric_limits<uint32_t>::max());
    op->rw.length = static_cast<uint32_t>(sub_txn_length / block_size);
    op->rw.vmo = vmo.get();
    op->rw.offset_dev = (off + sub_txn_offset) / block_size;
    op->rw.offset_vmo = (vmo_off + sub_txn_offset) / block_size;

    sync_completion_reset(&io_signal_);
    auto completion_cb = [](void* cookie, zx_status_t status, block_op_t* op) {
      BlockDevice* bdev = reinterpret_cast<BlockDevice*>(cookie);
      bdev->io_status_ = status;
      sync_completion_signal(&bdev->io_signal_);
    };

    BlockQueue(op, completion_cb, this);
    sync_completion_wait(&io_signal_, ZX_TIME_INFINITE);

    if (io_status_ != ZX_OK) {
      return io_status_;
    }

    sub_txn_offset += sub_txn_length;
  }

  return io_status_;
}

void BlockDevice::DdkRelease() { delete this; }

void BlockDevice::BlockQuery(block_info_t* block_info, size_t* op_size) {
  // It is important that all devices sitting on top of the volume protocol avoid
  // caching a copy of block info for query. The "block_count" field is dynamic,
  // and may change during the lifetime of the volume.
  size_t parent_op_size;
  parent_protocol_.Query(block_info, &parent_op_size);

  // Safety check that parent op size doesn't change dynamically.
  ZX_DEBUG_ASSERT(parent_op_size == parent_op_size_);

  *op_size = parent_op_size_;
}

void BlockDevice::BlockQueue(block_op_t* op, block_impl_queue_callback completion_cb,
                             void* cookie) {
  parent_protocol_.Queue(op, completion_cb, cookie);
}

void BlockDevice::GetInfo(GetInfoCompleter::Sync& completer) {
  fuchsia_hardware_block::wire::BlockInfo info;
  static_assert(sizeof(info) == sizeof(block_info_t));
  size_t block_op_size;
  parent_protocol_.Query(reinterpret_cast<block_info_t*>(&info), &block_op_size);
  // Set or clear fuchsia.hardware_block/Flag.BOOTPART appropriately.
  if (has_bootpart_) {
    info.flags |= fuchsia_hardware_block::wire::Flag::kBootpart;
  } else {
    info.flags -= fuchsia_hardware_block::wire::Flag::kBootpart;
  }
  completer.ReplySuccess(info);
}

void BlockDevice::OpenSession(OpenSessionRequestView request,
                              OpenSessionCompleter::Sync& completer) {
  CreateSession(std::move(request->session));
}

void BlockDevice::OpenSessionWithOffsetMap(OpenSessionWithOffsetMapRequestView request,
                                           OpenSessionWithOffsetMapCompleter::Sync& completer) {
  CreateSession(std::move(request->session), request->mapping);
}

void BlockDevice::CreateSession(
    fidl::ServerEnd<fuchsia_hardware_block::Session> session,
    std::optional<fuchsia_hardware_block::wire::BlockOffsetMapping> mapping) {
  zx::result server = Server::Create(&self_protocol_, mapping);
  if (server.is_error()) {
    session.Close(server.error_value());
    return;
  }

  // TODO(https://fxbug.dev/42067206): Avoid running a thread per session; make `Server` async
  // instead.
  thrd_t thread;
  if (thrd_create_with_name(
          &thread,
          +[](void* arg) {
            [[maybe_unused]] zx_status_t status = reinterpret_cast<Server*>(arg)->Serve();
            return 0;
          },
          server.value().get(), "block_server") != thrd_success) {
    session.Close(ZX_ERR_NO_MEMORY);
    return;
  }

  // Set a scheduling role for the block_server thread.
  // This is required in order to service the blobfs-pager-thread, which is on a deadline profile.
  // This will no longer be needed once we have the ability to propagate deadlines. Until then, we
  // need to set deadline profiles for all threads that the blobfs-pager-thread interacts with in
  // order to service page requests.
  //
  // Also note that this will apply to block_server threads spawned to service each block client
  // (in the typical case, we have two - blobfs and minfs). The capacity of 1ms is chosen so as to
  // accommodate most cases without throttling the thread. The desired capacity was 50us, but some
  // tests that use a large ramdisk require a larger capacity. In the average case though on a real
  // device, the block_server thread runs for less than 50us. 1ms provides us with a generous
  // leeway, without hurting performance in the typical case - a thread is not penalized for not
  // using its full capacity.
  {
    const char* role_name = "fuchsia.devices.block.drivers.core.block-server";
    if (zx_status_t status = device_set_profile_by_role(zxdev(), thrd_get_zx_handle(thread),
                                                        role_name, strlen(role_name));
        status != ZX_OK) {
      zxlogf(DEBUG, "block: Failed to apply role to block server: %s",
             zx_status_get_string(status));
    }
  }

  fidl::BindServer(
      fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(session),
      std::move(server.value()),
      [thread](Server* server, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_hardware_block::Session>) {
        server->Close();
        thrd_join(thread, nullptr);
      });
}

void BlockDevice::GetTypeGuid(GetTypeGuidCompleter::Sync& completer) {
  if (!parent_partition_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
    return;
  }
  fuchsia_hardware_block_partition::wire::Guid guid;
  static_assert(sizeof(guid.value) == sizeof(guid_t));
  guid_t* guid_ptr = reinterpret_cast<guid_t*>(guid.value.data());
  zx_status_t status = parent_partition_protocol_.GetGuid(GUIDTYPE_TYPE, guid_ptr);
  completer.Reply(status, fidl::ObjectView<decltype(guid)>::FromExternal(&guid));
}

void BlockDevice::GetInstanceGuid(GetInstanceGuidCompleter::Sync& completer) {
  if (!parent_partition_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
    return;
  }
  fuchsia_hardware_block_partition::wire::Guid guid;
  static_assert(sizeof(guid.value) == sizeof(guid_t));
  guid_t* guid_ptr = reinterpret_cast<guid_t*>(guid.value.data());
  zx_status_t status = parent_partition_protocol_.GetGuid(GUIDTYPE_INSTANCE, guid_ptr);
  completer.Reply(status, fidl::ObjectView<decltype(guid)>::FromExternal(&guid));
}

void BlockDevice::GetName(GetNameCompleter::Sync& completer) {
  if (!parent_partition_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
    return;
  }
  char name[fuchsia_hardware_block_partition::wire::kNameLength];
  zx_status_t status = parent_partition_protocol_.GetName(name, sizeof(name));
  completer.Reply(status,
                  status == ZX_OK ? fidl::StringView::FromExternal(name) : fidl::StringView{});
}

void BlockDevice::GetMetadata(GetMetadataCompleter::Sync& completer) {
  if (!parent_partition_protocol_.is_valid()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  partition_metadata_t metadata;
  if (zx_status_t status = parent_partition_protocol_.GetMetadata(&metadata); status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  static_assert(sizeof(fuchsia_hardware_block_partition::wire::Guid) == sizeof(guid_t));
  fuchsia_hardware_block_partition::wire::Guid type_guid, instance_guid;
  memcpy(type_guid.value.data(), &metadata.type_guid, sizeof(guid_t));
  memcpy(instance_guid.value.data(), &metadata.instance_guid, sizeof(guid_t));
  constexpr const fuchsia_hardware_block_partition::wire::Guid kNilGuid = {
      .value = {0},
  };

  fidl::Arena arena;
  auto response =
      fuchsia_hardware_block_partition::wire::PartitionGetMetadataResponse::Builder(arena);
  response.name(
      fidl::StringView::FromExternal(metadata.name, strnlen(metadata.name, sizeof(metadata.name))));
  if (type_guid.value != kNilGuid.value) {
    response.type_guid(type_guid);
  }
  if (instance_guid.value != kNilGuid.value) {
    response.instance_guid(instance_guid);
  }
  if (metadata.start_block_offset > 0) {
    response.start_block_offset(metadata.start_block_offset);
  }
  if (metadata.num_blocks > 0) {
    response.num_blocks(metadata.num_blocks);
  }
  response.flags(metadata.flags);
  completer.ReplySuccess(response.Build());
}

void BlockDevice::QuerySlices(QuerySlicesRequestView request,
                              QuerySlicesCompleter::Sync& completer) {
  if (!parent_volume_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {}, {});
    return;
  }
  fidl::Array<fuchsia_hardware_block_volume::wire::VsliceRange,
              fuchsia_hardware_block_volume::wire::kMaxSliceRequests>
      ranges;
  static_assert(sizeof(decltype(ranges)::value_type) == sizeof(slice_region_t));
  slice_region_t* ranges_ptr = reinterpret_cast<slice_region_t*>(ranges.data());
  size_t range_count;
  zx_status_t status = parent_volume_protocol_.QuerySlices(
      request->start_slices.data(), request->start_slices.count(), ranges_ptr, std::size(ranges),
      &range_count);
  completer.Reply(status, ranges, range_count);
}

void BlockDevice::GetVolumeInfo(GetVolumeInfoCompleter::Sync& completer) {
  if (!parent_volume_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {}, {});
    return;
  }
  fuchsia_hardware_block_volume::wire::VolumeManagerInfo manager_info;
  static_assert(sizeof(manager_info) == sizeof(volume_manager_info_t));
  fuchsia_hardware_block_volume::wire::VolumeInfo volume_info;
  static_assert(sizeof(volume_info) == sizeof(volume_info_t));
  zx_status_t status =
      parent_volume_protocol_.GetInfo(reinterpret_cast<volume_manager_info_t*>(&manager_info),
                                      reinterpret_cast<volume_info_t*>(&volume_info));
  fidl::ObjectView<decltype(manager_info)> manager_info_view;
  fidl::ObjectView<decltype(volume_info)> volume_info_view;
  if (status == ZX_OK) {
    manager_info_view = decltype(manager_info_view)::FromExternal(&manager_info);
    volume_info_view = decltype(volume_info_view)::FromExternal(&volume_info);
  }
  completer.Reply(status, manager_info_view, volume_info_view);
}

void BlockDevice::Extend(ExtendRequestView request, ExtendCompleter::Sync& completer) {
  if (!parent_volume_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  slice_extent_t extent = {
      .offset = request->start_slice,
      .length = request->slice_count,
  };
  completer.Reply(parent_volume_protocol_.Extend(&extent));
}

void BlockDevice::Shrink(ShrinkRequestView request, ShrinkCompleter::Sync& completer) {
  if (!parent_volume_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  slice_extent_t extent = {
      .offset = request->start_slice,
      .length = request->slice_count,
  };
  completer.Reply(parent_volume_protocol_.Shrink(&extent));
}

void BlockDevice::Destroy(DestroyCompleter::Sync& completer) {
  if (!parent_volume_protocol_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  completer.Reply(parent_volume_protocol_.Destroy());
}

zx_status_t BlockDevice::Bind(void* ctx, zx_device_t* dev) {
  auto bdev = std::make_unique<BlockDevice>(dev);

  // The Block Implementation Protocol is required.
  if (!bdev->parent_protocol_.is_valid()) {
    zxlogf(ERROR, "block device: does not support block protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  bdev->parent_protocol_.Query(&bdev->info_, &bdev->parent_op_size_);

  if (bdev->info_.max_transfer_size < bdev->info_.block_size) {
    zxlogf(ERROR, "block device: has smaller max xfer (0x%x) than block size (0x%x)",
           bdev->info_.max_transfer_size, bdev->info_.block_size);
    return ZX_ERR_NOT_SUPPORTED;
  }

  bdev->io_op_ = std::make_unique<uint8_t[]>(bdev->parent_op_size_);
  size_t block_size = bdev->info_.block_size;
  if ((block_size < 512) || (block_size & (block_size - 1))) {
    zxlogf(ERROR, "block device: invalid block size: %zu", block_size);
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Check to see if we have a ZBI partition map.
  uint8_t buffer[METADATA_PARTITION_MAP_MAX];
  size_t actual;
  zx_status_t status =
      device_get_metadata(dev, DEVICE_METADATA_PARTITION_MAP, buffer, sizeof(buffer), &actual);
  if (status == ZX_OK && actual >= sizeof(zbi_partition_map_t)) {
    bdev->has_bootpart_ = true;
  }

  // We implement |ZX_PROTOCOL_BLOCK|, not |ZX_PROTOCOL_BLOCK_IMPL|. This is the
  // "core driver" protocol for block device drivers.
  status = bdev->DdkAdd(ddk::DeviceAddArgs("block"));
  if (status != ZX_OK) {
    return status;
  }

  // The device has been added; we'll release it in blkdev_release.
  [[maybe_unused]] auto r = bdev.release();
  return ZX_OK;
}

static constexpr zx_driver_ops_t block_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = &BlockDevice::Bind;
  return ops;
}();

ZIRCON_DRIVER(block, block_driver_ops, "zircon", "0.1");
