// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_VIRTIO_GPU_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_VIRTIO_GPU_DEVICE_H_

#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/vector.h>

#include "src/graphics/display/drivers/virtio-gpu-display/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

// A virtual display exposed by a virtio-gpu device.
struct DisplayInfo {
  virtio_abi::ScanoutInfo scanout_info;
  int scanout_id;
};

// Implements the display-related subset of the virtio-gpu device specification.
class VirtioGpuDevice {
 public:
  explicit VirtioGpuDevice(std::unique_ptr<VirtioPciDevice> virtio_device);

  VirtioGpuDevice(const VirtioGpuDevice&) = delete;
  VirtioGpuDevice& operator=(const VirtioGpuDevice&) = delete;

  ~VirtioGpuDevice();

  // Updates the cursor.
  //
  // virtio13 5.7.6.10 "Device Operation: cursorq", operation
  // VIRTIO_GPU_CMD_UPDATE_CURSOR.
  zx::result<uint32_t> UpdateCursor();

  // Moves the cursor.
  //
  // virtio13 5.7.6.10 "Device Operation: cursorq", operation
  // VIRTIO_GPU_CMD_MOVE_CURSOR.
  zx::result<uint32_t> SetCursorPosition(uint32_t scanout_id, uint32_t x, uint32_t y);

  // Retrieves the current output configuration.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_GET_DISPLAY_INFO.
  zx::result<fbl::Vector<DisplayInfo>> GetDisplayInfo();

  // Creates a 2D resource on the virtio host.
  //
  // Returns the allocated resource ID. The returned ID is guaranteed to not
  // have been used for another active resource.
  //
  // This API does not currently support releasing resources, so every allocated
  // resource remains active for the driver's lifetime. However, the underlying
  // virtio spec does support releasing resources, via a
  // VIRTIO_GPU_CMD_RESOURCE_UNREF operation. So, this API may support releasing
  // resources in the future.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_CREATE_2D.
  zx::result<uint32_t> Create2DResource(uint32_t width, uint32_t height,
                                        display::PixelFormat pixel_format);

  // Sets scanout parameters for one scanout.
  //
  // Setting `resource_id` to kInvalidResourceId disables the scanout.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_SET_SCANOUT.
  zx::result<> SetScanoutProperties(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                                    uint32_t height);

  // Flushes any scanouts that use `resource_id` to the host screen.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_FLUSH.
  zx::result<> FlushResource(uint32_t resource_id, uint32_t width, uint32_t height);

  // Transfers data from a guest resource to host memory.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D.
  zx::result<> TransferToHost2D(uint32_t resource_id, uint32_t width, uint32_t height);

  // Assigns an array of guest pages as the backing store for a resource.
  //
  // virtio13 5.7.6.8 "Device Operation: controlq", operation
  // VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING.
  zx::result<> AttachResourceBacking(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len);

  const zx::bti& bti() { return virtio_device_->bti(); }

  VirtioPciDevice& pci_device() { return *virtio_device_.get(); }

 private:
  // Tracks the resource IDs allocated by Create2DResource().
  uint32_t next_resource_id_ = 1;

  const std::unique_ptr<VirtioPciDevice> virtio_device_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GPU_DISPLAY_VIRTIO_GPU_DEVICE_H_
