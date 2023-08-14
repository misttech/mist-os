// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_ABI_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_ABI_H_

// The constants and structures in this file are from the OASIS Virtual I/O
// Device (VIRTIO) specification, which can be downloaded from
// https://docs.oasis-open.org/virtio/virtio/
//
// virtio12 is Version 1.2, Committee Specification 01, dated 01 July 2022.

// We map the specification types "le32" and "le64" (little-endian 32/64-bit
// integers) to uint32_t and uint64_t, because Fuchsia only supports
// little-endian systems.
//
// We use static_asserts in the associated test file to ensure that our C++
// structure definitions are compatible with the C ABI specified by the spec.
// Concretely, we check that our structures have the same size (which implies
// the same packing) and a compatible alignment (same or larger) as the C
// structures defined by the specification.
//
// The specification uses "request" and "command" interchangeably. This header
// standardizes on "command". "request" must only be used when quoting the
// specification.

#include <cstdint>

namespace virtio_abi {

// GPU device configuration.
//
// struct virtio_gpu_config in virtio12 5.7.4 "Device configuration layout"
struct GpuDeviceConfig {
  using Events = uint32_t;

  // VIRTIO_GPU_EVENT_DISPLAY
  static constexpr Events kDisplayConfigChanged = 1 << 0;

  // The driver must not write to this field.
  Events pending_events;

  // Setting bits to one here clears the corresponding bits in `pending_events`.
  //
  // This works similarly to W/C (Write-Clear) registers in hardware.
  Events clear_events;

  // Maximum number of supported scanouts. Values must be in the range [1, 16].
  uint32_t scanout_limit;

  // Maximum number of supported capability sets. May be zero.
  uint32_t capability_set_limit;
};

// Type discriminant for driver commands and device responses.
//
// enum virtio_gpu_ctrl_type in virtio12 5.7.6.7 "Device Operation: Request
// header"
enum class ControlType : uint32_t {
  // Command encoded by `GetDisplayInfoCommand`.
  //
  // VIRTIO_GPU_CMD_GET_DISPLAY_INFO
  kGetDisplayInfoCommand = 0x0100,

  // Command encoded by `Create2DResourceCommand`.
  //
  // VIRTIO_GPU_CMD_RESOURCE_CREATE_2D
  kCreate2DResourceCommand = 0x0101,

  // VIRTIO_GPU_CMD_RESOURCE_UNREF
  kDestroyResourceCommand = 0x0102,

  // Command encoded by `SetScanoutCommand`.
  //
  // VIRTIO_GPU_CMD_SET_SCANOUT
  kSetScanoutCommand = 0x0103,

  // Command encoded by `FlushResourceCommand`.
  //
  // VIRTIO_GPU_CMD_RESOURCE_FLUSH
  kFlushResourceCommand = 0x0104,

  // Command encoded by `Transfer2DResourceToHostCommand`.
  //
  // VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D
  kTransfer2DResourceToHostCommand = 0x0105,

  // Command encoded by `AttachResourceBackingCommand`.
  //
  // VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING
  kAttachResourceBackingCommand = 0x0106,

  // VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING
  kDetachResourceBackingCommand = 0x0107,

  // VIRTIO_GPU_CMD_GET_CAPSET_INFO
  kGetCapabilitySetInfoCommand = 0x0108,

  // VIRTIO_GPU_CMD_GET_CAPSET
  kGetCapabilitySetCommand = 0x0109,

  // VIRTIO_GPU_CMD_GET_EDID
  kGetExtendedDisplayIdCommand = 0x010a,

  // VIRTIO_GPU_CMD_RESOURCE_ASSIGN_UUID
  kAssignResourceUuidCommand = 0x010b,

  // VIRTIO_GPU_CMD_CREATE_BLOB
  kCreateBlobCommand = 0x010c,

  // VIRTIO_GPU_CMD_SET_SCANOUT_BLOB
  kSetScanoutBlobCommand = 0x010d,

  // Response encoded by `EmptyResponse`.
  //
  // VIRTIO_GPU_RESP_OK_NODATA
  kEmptyResponse = 0x1100,

  // Response encoded by `DisplayInfoResponse`.
  //
  // VIRTIO_GPU_RESP_OK_DISPLAY_INFO
  kDisplayInfoResponse = 0x1101,

  // VIRTIO_GPU_RESP_OK_CAPSET_INFO
  kCapabilitySetInfoResponse = 0x1102,

  // VIRTIO_GPU_RESP_OK_CAPSET
  kCapabilitySetResponse = 0x1103,

  // VIRTIO_GPU_RESP_OK_EDID
  kExtendedDisplayIdResponse = 0x1104,

  // VIRTIO_GPU_RESP_OK_RESOURCE_UUID
  kResourceUuidResponse = 0x1105,

  // VIRTIO_GPU_RESP_OK_MAP_INFO
  kMapInfoResponse = 0x1106,

  // VIRTIO_GPU_RESP_ERR_UNSPEC
  kUnspecifiedError = 0x1200,

  // VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY
  kOutOfMemoryError = 0x1201,

  // VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID
  kInvalidScanoutIdError = 0x1202,

  // VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID
  kInvalidResourceIdError = 0x1203,

  // VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID
  kInvalidContextIdError = 0x1204,

  // VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER
  kInvalidParameterError = 0x1205,
};

// Descriptor for logging and debugging.
const char* ControlTypeToString(ControlType type);

// struct virtio_gpu_ctrl_hdr in virtio12 5.7.6.7 "Device Operation: Request
// header"
struct ControlHeader {
  using Flags = uint32_t;

  // See `fence_id` and `ring_index` for details.
  //
  // VIRTIO_GPU_FLAG_FENCE
  static constexpr Flags kFence = 1 << 0;

  // See `fence_id` and `ring_index` for details.
  //
  // VIRTIO_GPU_FLAG_INFO_RING_IDX
  static constexpr Flags kRingIndex = 1 << 1;

  ControlType type;

  Flags flags = 0;

  // Used for synchronization between the driver and the device.
  //
  // Only valid if the `kFence` bit is set in the `flags` field.
  //
  // The device must complete a command with the `kFence` flag set before
  // sending a response. The response must also have the `kFence` flag set, and
  // the same `fence_id`.
  uint64_t fence_id = 0;

  // Rendering context ID. Only used in 3D mode.
  uint32_t context_id = 0;

  // Points to a context-specific timeline for fences.
  //
  // Only valid if the `kRingIndex` and `kFence` bits are set in the `flags`
  // field. Values must be in the range [0, 63].
  uint8_t ring_index = 0;
};

// Encodes all driver-to-device commands that have no data besides the header.
struct EmptyCommand {
  ControlHeader header;
};

// Encodes all device-to-driver responses that have no data besides the header.
struct EmptyResponse {
  ControlHeader header;
};

// Populates a `DisplayInfoResponse` with the current output configuration.
using GetDisplayInfoCommand = EmptyCommand;

// struct virtio_gpu_rect in virtio12 5.7.6.8 "Device Operation: controlq",
// under the VIRTIO_GPU_CMD_GET_DISPLAY_INFO command description
struct ScanoutGeometry {
  // Placement relative to other displays.
  //
  // 0 is the left, axis points to the right.
  uint32_t placement_x;

  // Position relative to other displays.
  //
  // 0 is the top, axis points down.
  uint32_t placement_y;

  // The display's horizontal resolution, in pixels.
  uint32_t width;

  // The display's vertical resolution, in pixels.
  uint32_t height;
};

// struct virtio_gpu_display_one in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_GET_DISPLAY_INFO command description
struct ScanoutInfo {
  ScanoutGeometry geometry;

  // True as long as the display is "connected" (enabled by the user).
  //
  // This behaves similarly to the voltage level of the HPD (Hot-Plug Detect)
  // pin in connectors such as DisplayPort and HDMI. This is different from the
  // HPD interrupt generated by display hardware, which is triggered by changes
  // to the HPD pin voltage level.
  uint32_t enabled;

  // No flags are currently documented.
  uint32_t flags;
};

// VIRTIO_GPU_MAX_SCANOUTS in virtio12 5.7.6.8 "Device Operation: controlq",
// under the VIRTIO_GPU_CMD_GET_DISPLAY_INFO command description
constexpr int kMaxScanouts = 16;

// Response to a VIRTIO_GPU_CMD_GET_DISPLAY_INFO command.
//
// struct virtio_gpu_resp_display_info in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_GET_DISPLAY_INFO command description
struct DisplayInfoResponse {
  // `type` must be `kDisplayInfoResponse`.
  ControlHeader header;

  ScanoutInfo scanouts[kMaxScanouts];
};

// enum virtio_gpu_formats in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_RESOURCE_CREATE_2D command description
enum class ResourceFormat : uint32_t {
  // Equivalent to [`fuchsia.images2/PixelFormat.BGRA32`]
  //
  // VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM
  kBgra32 = 1,

  // VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
  kBgrx32 = 2,

  // VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM
  kArgb32 = 3,

  // VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM
  kXrgb32 = 4,

  // Equivalent to [`fuchsia.images2/PixelFormat.R8G8B8A8`].
  //
  // VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM
  kR8g8b8a8 = 67,

  // VIRTIO_GPU_FORMAT_X8B8G8R8_UNORM
  kXbgr32 = 68,

  // VIRTIO_GPU_FORMAT_A8B8G8R8_UNORM
  kAbgr32 = 121,

  // VIRTIO_GPU_FORMAT_A8B8G8R8_UNORM
  kRgbx32 = 134,
};

// struct virtio_gpu_resource_create_2d in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_RESOURCE_CREATE_2D command description
struct Create2DResourceCommand {
  // `type` must be `kCreate2DResourceCommand`.
  ControlHeader header;

  uint32_t resource_id;
  ResourceFormat format;
  uint32_t width;
  uint32_t height;
};

// Sets scanout parameters for a single output.
//
// The response does not have any data.
//
// struct virtio_gpu_set_scanout in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_SET_SCANOUT command description
struct SetScanoutCommand {
  // `type` must be `kSetScanoutCommand`.
  ControlHeader header;

  ScanoutGeometry geometry;
  uint32_t scanout_id;
  uint32_t resource_id;
};

// Flushes a scanout resource to the screen.
//
// The response does not have any data.
//
// struct virtio_gpu_resource_flush in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_RESOURCE_FLUSH command description
struct FlushResourceCommand {
  // `type` must be `kFlushResourceCommand`.
  ControlHeader header;

  ScanoutGeometry geometry;

  // Any scanouts that use this resource will be flushed.
  uint32_t resource_id;
};

// Flushes a scanout resource to the screen.
//
// The response does not have any data.
//
// struct virtio_gpu_transfer_to_host_2d in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D command description
struct Transfer2DResourceToHostCommand {
  // `type` must be `kTransfer2DResourceToHostCommand`.
  ControlHeader header;

  // Box to be transferred to the host.
  ScanoutGeometry geometry;

  uint64_t destination_offset;
  uint32_t resource_id;
};

// A continuous list of memory pages assigned to a 2D resource.
//
// The response does not have any data.
//
// struct virtio_gpu_mem_entry in virtio12 5.7.6.8 "Device Operation:
// controlq", under the VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING command
// description
struct MemoryEntry {
  uint64_t address;
  uint32_t length;
};

// Assigns backing pages to a resource.
//
// The response does not have any data.
//
// Typesafe combination of struct virtio_gpu_resource_attach_backing and
// struct virtio_gpu_mem_entry in virtio12 5.7.6.8 "Device Operation: controlq",
// under the VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING command
template <uint32_t N>
struct AttachResourceBackingCommand {
  // `type` must be `kAttachResourceBackingCommand`.
  ControlHeader header;
  uint32_t resource_id;
  uint32_t entry_count = N;
  MemoryEntry entries[N];
};

}  // namespace virtio_abi

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_ABI_H_
