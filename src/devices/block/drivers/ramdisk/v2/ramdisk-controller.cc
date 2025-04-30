// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/ramdisk/v2/ramdisk-controller.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <string>

#include <safemath/checked_math.h>

#include "src/devices/block/drivers/ramdisk/v2/ramdisk.h"

namespace ramdisk_v2 {

namespace fio = fuchsia_io;

RamdiskController::RamdiskController(fdf::DriverStartArgs start_args,
                                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("ramctl-v2", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> RamdiskController::Start() {
  fuchsia_hardware_ramdisk::Service::InstanceHandler handler(
      {.controller = bind_handler(dispatcher())});
  if (zx::result result =
          outgoing()->AddService<fuchsia_hardware_ramdisk::Service>(std::move(handler));
      result.is_error()) {
    return result;
  }
  inspector().Health().Ok();
  node_client_.Bind(std::move(node()), dispatcher());

  return zx::ok();
}

void RamdiskController::Create(CreateRequestView request, CreateCompleter::Sync& completer) {
  uint32_t block_size =
      request->has_block_size() ? request->block_size() : zx_system_get_page_size();
  if (block_size == 0) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  uint64_t block_count;
  zx::vmo vmo;
  if (request->has_vmo()) {
    vmo = std::move(request->vmo());
    if (request->has_block_count()) {
      block_count = request->block_count();

      if (!safemath::CheckMul<uint64_t>(block_size, block_count).IsValid()) {
        completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
        return;
      }
    } else {
      uint64_t vmo_size;
      if (zx_status_t status = vmo.get_size(&vmo_size); status != ZX_OK) {
        completer.Reply(zx::error(status));
        return;
      }
      block_count = vmo_size / block_size;
    }
  } else {
    if (!request->has_block_count()) {
      completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
      return;
    }
    block_count = request->block_count();

    uint64_t size;
    if (!safemath::CheckMul<uint64_t>(block_size, block_count).AssignIfValid(&size)) {
      completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
      return;
    }

    if (zx_status_t status = zx::vmo::create(size, 0, &vmo); status != ZX_OK) {
      completer.Reply(zx::error(status));
      return;
    }
  }
  component::OutgoingDirectory outgoing(dispatcher());

  fidl::ClientEnd<fio::Directory> client;
  zx::result server = fidl::CreateEndpoints(&client);
  if (server.is_error()) {
    completer.Reply(server.take_error());
    return;
  }
  if (zx::result result = outgoing.Serve(*std::move(server)); result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  block_server::PartitionInfo partition_info = {
      .start_block = 0,
      .block_count = block_count,
      .block_size = block_size,
  };
  if (request->has_max_transfer_blocks()) {
    partition_info.max_transfer_size = request->max_transfer_blocks() * block_size;
  }

  static std::atomic<int> counter = 0;

  int id = counter.fetch_add(1);
  if (request->has_type_guid())
    memcpy(partition_info.type_guid, request->type_guid().value.data(), 16);

  zx::eventpair endpoint0, endpoint1;
  if (zx_status_t status = zx::eventpair::create(0, &endpoint0, &endpoint1); status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }

  zx_handle_t handle = endpoint1.get();
  auto waiter = std::make_unique<async::WaitOnce>(handle, ZX_EVENTPAIR_PEER_CLOSED, 0);

  waiter->Begin(dispatcher(), [this, id, endpoint1 = std::move(endpoint1)](
                                  async_dispatcher_t*, async::WaitOnce*, zx_status_t,
                                  const zx_packet_signal_t*) { ramdisks_.erase(id); });

  if (zx::result ramdisk =
          Ramdisk::Create(this, dispatcher(), std::move(vmo), partition_info, std::move(outgoing),
                          id, request->has_publish() ? request->publish() : false);
      ramdisk.is_error()) {
    completer.Reply(ramdisk.take_error());
  } else {
    ramdisks_.emplace(id, std::make_pair(*std::move(ramdisk), std::move(waiter)));
    completer.ReplySuccess(std::move(client), std::move(endpoint0));
  }
}

}  // namespace ramdisk_v2

FUCHSIA_DRIVER_EXPORT(ramdisk_v2::RamdiskController);
