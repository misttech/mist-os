// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_SERVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_SERVER_H_

#include <fidl/fuchsia.gpu.virtio/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>

#include <functional>

namespace virtio_display {

class GpuControlServer : public fidl::WireServer<fuchsia_gpu_virtio::GpuControl> {
 public:
  class Owner {
   public:
    virtual void SendHardwareCommand(cpp20::span<uint8_t> request,
                                     std::function<void(cpp20::span<uint8_t>)> callback) = 0;
  };

  GpuControlServer(Owner* device_accessor, size_t capability_set_limit);

  zx::result<> Init(zx_device_t* parent_device);

  // virtio-gpu implementation
  void GetCapabilitySetLimit(GetCapabilitySetLimitCompleter::Sync& completer) override;
  void SendHardwareCommand(fuchsia_gpu_virtio::wire::GpuControlSendHardwareCommandRequest* request,
                           SendHardwareCommandCompleter::Sync& completer) override;

  Owner* owner() { return owner_; }

 private:
  zx::result<zx::vmo> GetCapset(uint32_t capset_id, uint32_t capset_version);

  Owner* owner_;
  component::OutgoingDirectory outgoing_;
  size_t capability_set_limit_;

  fidl::ServerBindingGroup<fuchsia_gpu_virtio::GpuControl> bindings_;
  zx_device_t* gpu_control_device_ = nullptr;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_GPU_CONTROL_SERVER_H_
