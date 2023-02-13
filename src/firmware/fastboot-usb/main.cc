// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.fastboot/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fastboot/fastboot.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/syscalls.h>

#include "src/lib/fsl/io/device_watcher.h"

constexpr char kUsbFastbootDirectory[] = "/dev/class/fastboot";

zx::result<fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl>>
ConnectToUsbFastbootDevice() {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::ClientEnd<fuchsia_hardware_fastboot::FastbootImpl> client;
  auto watcher = fsl::DeviceWatcher::Create(
      kUsbFastbootDirectory,
      [&](int dir_fd, const std::string &filename) {
        if (client.is_valid()) {
          FX_LOGS(INFO) << "There's already a connected client. Ignoring new devices";
          return;
        }

        FX_LOGS(INFO) << "Connecting to " << kUsbFastbootDirectory << "/" << filename.data();
        fdio_cpp::UnownedFdioCaller caller(dir_fd);
        auto client_end = component::ConnectAt<fuchsia_hardware_fastboot::FastbootImpl>(
            caller.directory(), filename.c_str());
        if (!client_end.is_ok()) {
          FX_LOGS(INFO) << "Fail to call connect to driver";
          return;
        }

        client = std::move(client_end.value());
        loop.Quit();
      },
      loop.dispatcher());
  if (!watcher) {
    FX_LOGS(ERROR) << "Could not create device watcher";
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  loop.Run();

  return zx::ok(fidl::WireSyncClient(std::move(client)));
}

class UsbPacketTransport : public fastboot::Transport {
 public:
  UsbPacketTransport(fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> &device,
                     std::string_view packet)
      : device_(&device), packet_(packet) {}

  zx::result<size_t> ReceivePacket(void *dst, size_t capacity) override {
    if (capacity < PeekPacketSize()) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    memcpy(dst, packet_.data(), packet_.size());
    return zx::ok(packet_.size());
  }

  size_t PeekPacketSize() override { return packet_.size(); }

  zx::result<> Send(std::string_view packet) override {
    fzl::OwnedVmoMapper mapper;
    zx_status_t status = mapper.CreateAndMap(packet.size(), "fastboot usb send");
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to create vmo mapper for sending " << zx_status_get_string(status);
      return zx::error(status);
    }
    memcpy(mapper.start(), packet.data(), packet.size());

    if (zx_status_t status = mapper.vmo().set_prop_content_size(packet.size()); status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to set content size " << zx_status_get_string(status);
      return zx::error(status);
    }

    auto res = (*device_)->Send(mapper.Release());
    if (res->is_error()) {
      return res->take_error();
    }
    return zx::ok();
  }

 private:
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> *device_ = nullptr;
  std::string_view packet_;
};

int main(int argc, const char **argv) {
  FX_LOGS(INFO) << "Starting fastboot usb";
  auto connect_device = ConnectToUsbFastbootDevice();
  if (connect_device.is_error()) {
    FX_LOGS(ERROR) << "Failed to connect to usb fastboot device" << connect_device.status_string();
    return 1;
  }
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> device =
      std::move(connect_device.value());

  fastboot::Fastboot fastboot(zx_system_get_physmem());
  while (true) {
    // Note: fastboot.remaining_download_size() returns 0 in command stage.
    size_t request_size =
        std::min(fastboot.remaining_download_size(), static_cast<size_t>(512 * 1024));
    auto packet_res = device->Receive(request_size);
    if (packet_res->is_error()) {
      FX_LOGS(ERROR) << "Failed while receiving packet " << packet_res.status_string();
      continue;
    }

    fzl::VmoMapper mapper;
    zx_status_t status = mapper.Map(packet_res.value()->data);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to map packet vmo" << zx_status_get_string(status);
      continue;
    }

    size_t packet_size = 0;
    if (zx_status_t status = packet_res.value()->data.get_prop_content_size(&packet_size);
        status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to get content size " << zx_status_get_string(status);
      continue;
    }

    UsbPacketTransport transport(
        device, std::string_view{reinterpret_cast<const char *>(mapper.start()), packet_size});
    auto fastboot_res = fastboot.ProcessPacket(&transport);
    if (fastboot_res.is_error()) {
      FX_LOGS(ERROR) << "Failed to process fastboot packet " << fastboot_res.status_string();
    }
  }

  return 0;
}
