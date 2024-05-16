// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdlib>
#include <limits>
#include <memory>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

namespace virtio_display {

// Implements the guest OS driver side of the VIRTIO GPU device specification.
class GpuDevice : public fdf::WireServer<fuchsia_hardware_display_engine::Engine> {
 public:
  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `bti` is used to obtain physical memory addresses given to the virtio
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
  GpuDevice(zx::vmo virtio_queue_buffer_pool_vmo, zx::pmt virtio_queue_buffer_pool_pin,
            zx_paddr_t virtio_queue_buffer_pool_physical_address,
            cpp20::span<uint8_t> virtio_queue_buffer_pool);
  ~GpuDevice() override;

  static zx::result<std::unique_ptr<GpuDevice>> Create(
      fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end);

  // `fuchsia.hardware.display.engine/Engine` implementation
  void ImportBufferCollection(ImportBufferCollectionRequestView request, fdf::Arena& arena,
                              ImportBufferCollectionCompleter::Sync& completer) override {}
  void ReleaseBufferCollection(ReleaseBufferCollectionRequestView request, fdf::Arena& arena,
                               ReleaseBufferCollectionCompleter::Sync& completer) override {}
  void ImportImage(ImportImageRequestView request, fdf::Arena& arena,
                   ImportImageCompleter::Sync& completer) override {}
  void ImportImageForCapture(ImportImageForCaptureRequestView request, fdf::Arena& arena,
                             ImportImageForCaptureCompleter::Sync& completer) override {}
  void ReleaseImage(ReleaseImageRequestView request, fdf::Arena& arena,
                    ReleaseImageCompleter::Sync& completer) override {}
  void CheckConfiguration(CheckConfigurationRequestView request, fdf::Arena& arena,
                          CheckConfigurationCompleter::Sync& completer) override {}
  void ApplyConfiguration(ApplyConfigurationRequestView request, fdf::Arena& arena,
                          ApplyConfigurationCompleter::Sync& completer) override {}
  void SetBufferCollectionConstraints(
      SetBufferCollectionConstraintsRequestView request, fdf::Arena& arena,
      SetBufferCollectionConstraintsCompleter::Sync& completer) override {}
  void SetDisplayPower(SetDisplayPowerRequestView request, fdf::Arena& arena,
                       SetDisplayPowerCompleter::Sync& completer) override {}
  void SetMinimumRgb(SetMinimumRgbRequestView request, fdf::Arena& arena,
                     SetMinimumRgbCompleter::Sync& completer) override {}
  void IsCaptureSupported(fdf::Arena& arena,
                          IsCaptureSupportedCompleter::Sync& completer) override {}
  void StartCapture(StartCaptureRequestView request, fdf::Arena& arena,
                    StartCaptureCompleter::Sync& completer) override {}
  void ReleaseCapture(ReleaseCaptureRequestView request, fdf::Arena& arena,
                      ReleaseCaptureCompleter::Sync& completer) override {}
  void IsCaptureCompleted(fdf::Arena& arena,
                          IsCaptureCompletedCompleter::Sync& completer) override {}
  void IsAvailable(fdf::Arena& arena, IsAvailableCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_display_engine::Engine> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  zx_status_t Init();
  const static char* tag() { return "virtio-gpu"; }

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
  //
  // The current implementation fully serializes request/response exchanges.
  // More precisely, even if ExchangeRequestResponse() is called concurrently,
  // the virtio device will never be given a request while another request is
  // pending. While this behavior is observable, it is not part of the API. The
  // implementation may be evolved to issue concurrent requests in the future.
  template <typename ResponseType, typename RequestType>
  const ResponseType& ExchangeRequestResponse(const RequestType& request);

 private:
  // Ensures that a single ExchangeRequestResponse() call is in progress.
  fbl::Mutex exchange_request_response_mutex_;

  // Protects data members modified by multiple threads.
  fbl::Mutex virtio_queue_mutex_;

  // Signaled when the virtio device consumes a designated virtio queue buffer.
  //
  // The buffer is identified by `virtio_queue_request_index_`.
  fbl::ConditionVariable virtio_queue_buffer_used_signal_;

  // Identifies the request buffer allocated in ExchangeRequestResponse().
  //
  // nullopt iff no ExchangeRequestResponse() is in progress.
  std::optional<uint16_t> virtio_queue_request_index_ __TA_GUARDED(virtio_queue_mutex_);

  // Backs `virtio_queue_buffer_pool_`.
  const zx::vmo virtio_queue_buffer_pool_vmo_;

  // Pins `virtio_queue_buffer_pool_vmo_` at a known physical address.
  const zx::pmt virtio_queue_buffer_pool_pin_;

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

  // Request/response exchanges are fully serialized.
  //
  // Relaxing this implementation constraint would require the following:
  // * An allocation scheme for `virtual_queue_buffer_pool`. An easy solution is
  //   to sub-divide the area into equally-sized cells, where each cell can hold
  //   the largest possible request + response.
  // * A map of virtio queue descriptor index to condition variable, so multiple
  //   threads can be waiting on the device notification interrupt.
  // * Handling the case where `virtual_queue_buffer_pool` is full. Options are
  //   waiting on a new condition variable signaled whenever a buffer is
  //   released, and asserting that implies the buffer pool is statically sized
  //   to handle the maximum possible concurrency.
  //
  // Optionally, the locking around `virtio_queue_mutex_` could be more
  // fine-grained. Allocating the descriptors needs to be serialized, but
  // populating them can be done concurrently. Last, submitting the descriptors
  // to the virtio device must be serialized.
  fbl::AutoLock exhange_request_response_lock(&exchange_request_response_mutex_);

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
  virtio_queue_request_index_ = request_descriptor_index;

  cpp20::span<uint8_t> request_span = virtio_queue_buffer_pool_.subspan(0, request_size);
  std::memcpy(request_span.data(), &request, request_size);

  cpp20::span<uint8_t> response_span =
      virtio_queue_buffer_pool_.subspan(request_size, response_size);
  std::fill(response_span.begin(), response_span.end(), 0);
  static_assert(response_size <= std::numeric_limits<uint32_t>::max());

  return *reinterpret_cast<ResponseType*>(response_span.data());
}

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V2_GPU_DEVICE_H_
