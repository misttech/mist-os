// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-partition-device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <string.h>
#include <zircon/hw/gpt.h>

#include <bind/fuchsia/cpp/bind.h>

#include "sdmmc-block-device.h"
#include "sdmmc-root-device.h"
#include "sdmmc-types.h"

namespace sdmmc {

PartitionDevice::PartitionDevice(SdmmcBlockDevice* sdmmc_parent, const block_info_t& block_info,
                                 EmmcPartition partition)
    : sdmmc_parent_(sdmmc_parent), block_info_(block_info), partition_(partition) {
  block_server::PartitionInfo info{
      .block_count = block_info.block_count,
      .block_size = block_info.block_size,
  };
  switch (partition_) {
    case USER_DATA_PARTITION: {
      // For compatibility with the old implementation, don't give 'user' a visible name/guid.
      partition_name_ = "user";
      break;
    }
    case BOOT_PARTITION_1: {
      partition_name_ = "boot1";
      info.name = partition_name_;
      const uint8_t guid[16] = GUID_EMMC_BOOT1_VALUE;
      std::copy_n(guid, sizeof(guid), std::begin(info.type_guid));
      break;
    }
    case BOOT_PARTITION_2: {
      partition_name_ = "boot2";
      info.name = partition_name_;
      const uint8_t guid[16] = GUID_EMMC_BOOT2_VALUE;
      std::copy_n(guid, sizeof(guid), std::begin(info.type_guid));
      break;
    }
    default:
      // partition_name_ is left empty, which causes PartitionDevice::AddDevice() to return an
      // error.
      break;
  }
  block_server_.emplace(info, this);
}

zx_status_t PartitionDevice::AddDevice() {
  {
    const std::string path_from_parent = std::string(sdmmc_parent_->parent()->driver_name()) + "/" +
                                         std::string(sdmmc_parent_->block_name()) + "/";
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_BLOCK_IMPL] = block_impl_server_.callback();
    if (partition_ != USER_DATA_PARTITION) {
      block_partition_server_.emplace(ZX_PROTOCOL_BLOCK_PARTITION, this,
                                      &block_partition_protocol_ops_);
      banjo_config.callbacks[ZX_PROTOCOL_BLOCK_PARTITION] = block_partition_server_->callback();
    }

    auto result = compat_server_.Initialize(
        sdmmc_parent_->parent()->driver_incoming(), sdmmc_parent_->parent()->driver_outgoing(),
        sdmmc_parent_->parent()->driver_node_name(), partition_name_,
        compat::ForwardMetadata::Some({DEVICE_METADATA_GPT_INFO}), std::move(banjo_config),
        path_from_parent);
    if (result.is_error()) {
      return result.status_value();
    }
  }

  if (!partition_name_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia::PROTOCOL,
                                    static_cast<uint32_t>(ZX_PROTOCOL_BLOCK_IMPL));

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, partition_name_)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result = sdmmc_parent_->block_node()->AddChild(args, std::move(controller_server_end), {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child partition device: %s", result.status_string());
    return result.status();
  }

  if (zx::result result =
          sdmmc_parent_->parent()
              ->driver_outgoing()
              ->AddService<fuchsia_hardware_block_volume::Service>(
                  fuchsia_hardware_block_volume::Service::InstanceHandler({
                      .volume =
                          [this](
                              fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
                            fbl::AutoLock lock(&lock_);
                            if (block_server_)
                              block_server_->Serve(std::move(server_end));
                          },
                  }),
                  partition_name_);
      result.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to add service instance for '%s': %s", partition_name_,
             result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

void PartitionDevice::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  memcpy(info_out, &block_info_, sizeof(*info_out));
  *block_op_size_out = BlockOperation::OperationSize(sizeof(block_op_t));
}

void PartitionDevice::BlockImplQueue(block_op_t* btxn, block_impl_queue_callback completion_cb,
                                     void* cookie) {
  BlockOperation txn(btxn, completion_cb, cookie, sizeof(block_op_t));
  txn.private_storage()->partition = partition_;
  txn.private_storage()->block_count = block_info_.block_count;
  sdmmc_parent_->Queue(std::move(txn));
}

zx_status_t PartitionDevice::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  ZX_DEBUG_ASSERT(partition_ != USER_DATA_PARTITION);

  constexpr uint8_t kGuidEmmcBoot1Value[] = GUID_EMMC_BOOT1_VALUE;
  constexpr uint8_t kGuidEmmcBoot2Value[] = GUID_EMMC_BOOT2_VALUE;

  switch (guid_type) {
    case GUIDTYPE_TYPE:
      if (partition_ == BOOT_PARTITION_1) {
        memcpy(&out_guid->data1, kGuidEmmcBoot1Value, GUID_LENGTH);
      } else {
        memcpy(&out_guid->data1, kGuidEmmcBoot2Value, GUID_LENGTH);
      }
      return ZX_OK;
    case GUIDTYPE_INSTANCE:
      return ZX_ERR_NOT_SUPPORTED;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
}

zx_status_t PartitionDevice::BlockPartitionGetName(char* out_name, size_t capacity) {
  ZX_DEBUG_ASSERT(partition_ != USER_DATA_PARTITION);
  if (capacity <= strlen(partition_name_)) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  strlcpy(out_name, partition_name_, capacity);

  return ZX_OK;
}

zx_status_t PartitionDevice::BlockPartitionGetMetadata(partition_metadata_t* out_metadata) {
  strlcpy(out_metadata->name, partition_name_, sizeof(out_metadata->name));
  if (zx_status_t status = BlockPartitionGetGuid(GUIDTYPE_TYPE, &out_metadata->type_guid);
      status != ZX_OK) {
    return status;
  }
  memset(&out_metadata->instance_guid, 0, sizeof(out_metadata->instance_guid));
  out_metadata->start_block_offset = 0;
  out_metadata->num_blocks = block_info_.block_count;
  out_metadata->flags = 0;
  return ZX_OK;
}

fdf::Logger& PartitionDevice::logger() const { return sdmmc_parent_->logger(); }

void PartitionDevice::StopBlockServer() {
  fbl::AutoLock lock(&lock_);
  if (block_server_) {
    std::move(block_server_).value().DestroyAsync([]() {});
  }
}

void PartitionDevice::StartThread(block_server::Thread thread) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "SDMMC Block Server",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [thread = std::move(thread)]() mutable { thread.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

void PartitionDevice::OnNewSession(block_server::Session session) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "Block Server Session",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [session = std::move(session)]() mutable { session.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

void PartitionDevice::OnRequests(const block_server::Session& session,
                                 cpp20::span<block_server::Request> requests) {
  sdmmc_parent_->OnRequests(session, *this, requests);
}

}  // namespace sdmmc
