// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fit/defer.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/bti.h>
#include <lib/zx/vmo.h>
#include <semaphore.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <cstdlib>
#include <memory>

namespace virtio_display {

// Implements the guest OS driver side of the VIRTIO GPU device specification.
class GpuDevice : public virtio::Device {
 public:
  GpuDevice(zx::bti bti, std::unique_ptr<virtio::Backend> backend);
  ~GpuDevice() override;

  static fit::result<zx_status_t, std::unique_ptr<GpuDevice>> Create(
      fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end);

  zx_status_t Init() override;
  void IrqRingUpdate() override;
  void IrqConfigChange() override;
  const char* tag() const override { return "virtio-gpu"; }

  static uint64_t GetRequestSize(zx::vmo& vmo);

  template <typename RequestType, typename ResponseType>
  void send_command_response(const RequestType* cmd, ResponseType** res);

 private:
  sem_t request_sem_ = {};
  sem_t response_sem_ = {};
  virtio::Ring vring_ = {this};
  zx::vmo request_vmo_;
  zx::pmt request_pmt_;
  zx_paddr_t request_phys_addr_ = {};
  zx_vaddr_t request_virt_addr_ = {};
  std::optional<uint32_t> capset_count_;
};

template <typename RequestType, typename ResponseType>
void GpuDevice::send_command_response(const RequestType* cmd, ResponseType** res) {
  size_t cmd_len = sizeof(RequestType);
  size_t res_len = sizeof(ResponseType);
  FDF_LOG(INFO,
          "Sending command (buffer at %p, length %zu), expecting reply (pointer at %p, length %zu)",
          cmd, cmd_len, res, res_len);

  // Keep this single message at a time
  sem_wait(&request_sem_);
  auto cleanup = fit::defer([this]() { sem_post(&request_sem_); });

  uint16_t i;
  struct vring_desc* desc = vring_.AllocDescChain(2, &i);
  ZX_ASSERT(desc);

  auto gpu_req_base = reinterpret_cast<void*>(request_virt_addr_);
  zx_paddr_t gpu_req_pa = request_phys_addr_;

  memcpy(gpu_req_base, cmd, cmd_len);

  desc->addr = gpu_req_pa;
  desc->len = static_cast<uint32_t>(cmd_len);
  desc->flags = VRING_DESC_F_NEXT;

  // Set the second descriptor to the response with the write bit set
  desc = vring_.DescFromIndex(desc->next);
  ZX_ASSERT(desc);

  *res = reinterpret_cast<ResponseType*>(static_cast<uint8_t*>(gpu_req_base) + cmd_len);
  zx_paddr_t res_phys = gpu_req_pa + cmd_len;
  memset(*res, 0, res_len);

  desc->addr = res_phys;
  desc->len = static_cast<uint32_t>(res_len);
  desc->flags = VRING_DESC_F_WRITE;

  // Submit the transfer & wait for the response
  vring_.SubmitChain(i);
  vring_.Kick();
  sem_wait(&response_sem_);
}

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
