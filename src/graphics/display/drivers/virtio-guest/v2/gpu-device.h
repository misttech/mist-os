// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdlib>
#include <limits>
#include <memory>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

namespace virtio_display {

// Implements the guest OS driver side of the VIRTIO GPU device specification.
class GpuDevice : public virtio::Device {
 public:
  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `bti` is used to obtain phyiscal memory addresses given to the virtio
  // device.
  //
  // `virtio_queue_buffer_pool` must be large enough to store requests and
  // responses exchanged with the virtio device. The buffer must reside at
  // `virtio_queue_buffer_pool_physical_address` in the virtio device's physical
  // address space.
  //
  // The instance hangs onto `virtio_queue_buffer_pool_vmo` and
  // `virtio_queue_buffer_pool_pin` for the duration of its lifetime. They are
  // intended to keep the memory backing `virtio_queue_buffer_pool` alive and
  // pinned to `virtio_queue_buffer_pool_physical_address`.
  GpuDevice(zx::bti bti, std::unique_ptr<virtio::Backend> backend,
            zx::vmo virtio_queue_buffer_pool_vmo, zx::pmt virtio_queue_buffer_pool_pin,
            zx_paddr_t virtio_queue_buffer_pool_physical_address,
            cpp20::span<uint8_t> virtio_queue_buffer_pool);
  ~GpuDevice() override;

  static zx::result<std::unique_ptr<GpuDevice>> Create(
      fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end);

  zx_status_t Init() override;
  void IrqRingUpdate() override;
  void IrqConfigChange() override;
  const char* tag() const override { return "virtio-gpu"; }

  // Synchronous request/response exchange on the main virtqueue.
  //
  // The returned reference points to data owned by the GpuDevice instance, and
  // is only valid until the method is called again.
  //
  // Call sites are expected to rely on partial template type inference. The
  // argument type should not be specified twice.
  //
  //     const virtio_abi::EmptyRequest request = {...};
  //     const auto& response =
  //         device->ExchangeRequestResponse<virtio_abi::EmptyResponse>(
  //             request);
  template <typename ResponseType, typename RequestType>
  const ResponseType& ExchangeRequestResponse(const RequestType& request);

 private:
  fbl::Mutex virtio_queue_mutex_;
  fbl::ConditionVariable virtio_queue_buffer_used_signal_;

  // The GPU device's control virtqueue.
  //
  // Defined in the VIRTIO spec Section 5.7.2 "GPU Device" > "Virtqueues".
  virtio::Ring virtio_queue_;

  // Backs `virtio_queue_buffer_pool_`.
  const zx::vmo virtio_queue_buffer_pool_vmo_;

  // Pins `virtio_queue_buffer_pool_vmo_` at a known physical address.
  const zx::pmt virtio_queue_buffer_pool_pin_;

  // The starting address of `virtio_queue_buffer_pool_`.
  const zx_paddr_t virtio_queue_buffer_pool_physical_address_;

  // Memory pinned at a known physical address, used for virtqueue buffers.
  //
  // The span's data is modified by the driver and by the virtio device.
  const cpp20::span<uint8_t> virtio_queue_buffer_pool_;

  std::optional<uint32_t> capset_count_;
};

template <typename ResponseType, typename RequestType>
const ResponseType& GpuDevice::ExchangeRequestResponse(const RequestType& request) {
  static constexpr size_t request_size = sizeof(RequestType);
  static constexpr size_t response_size = sizeof(ResponseType);
  FDF_LOG(TRACE, "Sending %zu-byte request, expecting %zu-byte response", request_size,
          response_size);

  // Keep this single message at a time
  fbl::AutoLock virtio_queue_lock(&virtio_queue_mutex_);

  // Allocate two virtqueue descriptors. This is the minimum number of
  // descriptors needed to represent a request / response exchange using the
  // split virtqueue format described in the VIRTIO spec Section 2.7 "Split
  // Virtqueues". This is because each descriptor can point to a read-only or a
  // write-only memory buffer, and we need one of each.
  //
  // The first (returned) descriptor will point to the request buffer, and the
  // second (chained) descriptor will point to the response buffer.

  uint16_t request_descriptor_index;
  vring_desc* const request_descriptor =
      virtio_queue_.AllocDescChain(/*count=*/2, &request_descriptor_index);
  ZX_ASSERT(request_descriptor);

  cpp20::span<uint8_t> request_span = virtio_queue_buffer_pool_.subspan(0, request_size);
  std::memcpy(request_span.data(), &request, request_size);

  const zx_paddr_t request_physical_address = virtio_queue_buffer_pool_physical_address_;
  request_descriptor->addr = request_physical_address;
  static_assert(request_size <= std::numeric_limits<uint32_t>::max());
  request_descriptor->len = static_cast<uint32_t>(request_size);
  request_descriptor->flags = VRING_DESC_F_NEXT;

  vring_desc* const response_descriptor = virtio_queue_.DescFromIndex(request_descriptor->next);
  ZX_ASSERT(response_descriptor);

  cpp20::span<uint8_t> response_span =
      virtio_queue_buffer_pool_.subspan(request_size, response_size);
  std::fill(response_span.begin(), response_span.end(), 0);

  const zx_paddr_t response_physical_address = request_physical_address + request_size;
  response_descriptor->addr = response_physical_address;
  static_assert(response_size <= std::numeric_limits<uint32_t>::max());
  response_descriptor->len = static_cast<uint32_t>(response_size);
  response_descriptor->flags = VRING_DESC_F_WRITE;

  // Submit the transfer & wait for the response
  virtio_queue_.SubmitChain(request_descriptor_index);
  virtio_queue_.Kick();

  virtio_queue_buffer_used_signal_.Wait(&virtio_queue_mutex_);

  return *reinterpret_cast<ResponseType*>(response_span.data());
}

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
