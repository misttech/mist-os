// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/block_client/cpp/remote_block_device.h"

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>

namespace block_client {

zx_status_t RemoteBlockDevice::FifoTransaction(block_fifo_request_t* requests, size_t count) {
  return fifo_client_.Transaction(requests, count);
}

zx::result<std::string> RemoteBlockDevice::GetTopologicalPath() const {
  const fidl::WireResult result = fidl::WireCall(controller_)->GetTopologicalPath();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(response->path.get());
}

zx::result<> RemoteBlockDevice::Rebind(std::string_view url_suffix) const {
  const fidl::WireResult result =
      fidl::WireCall(controller_)->Rebind(fidl::StringView::FromExternal(url_suffix));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok();
}

zx_status_t RemoteBlockDevice::BlockGetInfo(
    fuchsia_hardware_block::wire::BlockInfo* out_info) const {
  const fidl::WireResult result = fidl::WireCall(device_)->GetInfo();
  if (!result.ok()) {
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  *out_info = response.value()->info;
  return ZX_OK;
}

zx_status_t RemoteBlockDevice::BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out_vmoid) {
  zx::result vmoid = fifo_client_.RegisterVmo(vmo);
  if (vmoid.is_error()) {
    return vmoid.error_value();
  }
  *out_vmoid = std::move(vmoid.value());
  return ZX_OK;
}

zx_status_t RemoteBlockDevice::VolumeGetInfo(
    fuchsia_hardware_block_volume::wire::VolumeManagerInfo* out_manager_info,
    fuchsia_hardware_block_volume::wire::VolumeInfo* out_volume_info) const {
  const fidl::WireResult result = fidl::WireCall(device_)->GetVolumeInfo();
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    return status;
  }
  *out_manager_info = *response.manager;
  *out_volume_info = *response.volume;
  return ZX_OK;
}

zx_status_t RemoteBlockDevice::VolumeQuerySlices(
    const uint64_t* slices, size_t slices_count,
    fuchsia_hardware_block_volume::wire::VsliceRange* out_ranges, size_t* out_ranges_count) const {
  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> volume(device_.channel().borrow());
  const fidl::WireResult result = fidl::WireCall(volume)->QuerySlices(
      fidl::VectorView<uint64_t>::FromExternal(const_cast<uint64_t*>(slices), slices_count));
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    return status;
  }
  std::copy_n(response.response.data(), response.response_count, out_ranges);
  *out_ranges_count = response.response_count;
  return ZX_OK;
}

zx_status_t RemoteBlockDevice::VolumeExtend(uint64_t offset, uint64_t length) {
  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> volume(device_.channel().borrow());
  const fidl::WireResult result = fidl::WireCall(volume)->Extend(offset, length);
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  return response.status;
}

zx_status_t RemoteBlockDevice::VolumeShrink(uint64_t offset, uint64_t length) {
  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> volume(device_.channel().borrow());
  const fidl::WireResult result = fidl::WireCall(volume)->Shrink(offset, length);
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  return response.status;
}

zx::result<std::unique_ptr<RemoteBlockDevice>> RemoteBlockDevice::Create(
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> device,
    fidl::ClientEnd<fuchsia_device::Controller> controller) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_block::Session>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto& [session, server] = endpoints.value();
  if (fidl::Status result = fidl::WireCall(device)->OpenSession(std::move(server)); !result.ok()) {
    return zx::error(result.status());
  }
  const fidl::WireResult result = fidl::WireCall(session)->GetFifo();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(std::unique_ptr<RemoteBlockDevice>(new RemoteBlockDevice(
      std::move(device), std::move(controller), std::move(session), std::move(response->fifo))));
}

RemoteBlockDevice::RemoteBlockDevice(fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> device,
                                     fidl::ClientEnd<fuchsia_device::Controller> controller,
                                     fidl::ClientEnd<fuchsia_hardware_block::Session> session,
                                     zx::fifo fifo)
    : device_(std::move(device)),
      controller_(std::move(controller)),
      fifo_client_(std::move(session), std::move(fifo)) {}

zx_status_t ReadWriteBlocks(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device,
                            void* buffer, size_t buffer_length, size_t offset, bool write) {
  // Get the Block info for block size calculations:
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  if (!result.ok()) {
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }

  zx::vmo vmo;
  if (zx_status_t status = zx::vmo::create(buffer_length, 0, &vmo); status != ZX_OK) {
    return status;
  }

  size_t block_size = response.value()->info.block_size;
  if (!buffer || buffer_length % block_size != 0 || offset % block_size != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx::vmo read_vmo;
  if (write) {
    if (zx_status_t status = vmo.write(buffer, 0, buffer_length); status != ZX_OK) {
      return status;
    }
    const fidl::WireResult result =
        fidl::WireCall(device)->WriteBlocks(std::move(vmo), buffer_length, offset, 0);
    if (!result.ok()) {
      return result.status();
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
  } else {
    // if reading, duplicate the vmo so we will retain a copy
    if (zx_status_t status = vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &read_vmo); status != ZX_OK) {
      return status;
    }
    const fidl::WireResult result =
        fidl::WireCall(device)->ReadBlocks(std::move(vmo), buffer_length, offset, 0);
    if (!result.ok()) {
      return result.status();
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
  }

  if (!write) {
    return read_vmo.read(buffer, 0, buffer_length);
  }
  return ZX_OK;
}

fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> RemoteBlockDevice::TakeDevice() {
  return std::move(device_);
}

zx_status_t SingleReadBytes(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device,
                            void* buffer, size_t buffer_size, size_t offset) {
  return ReadWriteBlocks(device, buffer, buffer_size, offset, false);
}

zx_status_t SingleWriteBytes(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device,
                             void* buffer, size_t buffer_size, size_t offset) {
  return ReadWriteBlocks(device, buffer, buffer_size, offset, true);
}
}  // namespace block_client
