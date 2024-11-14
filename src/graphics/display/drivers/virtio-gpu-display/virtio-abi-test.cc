// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/lib/virtio/virtio-abi.h"

#include <cstddef>
#include <type_traits>

namespace virtio_abi {

static_assert(std::is_standard_layout_v<GpuDeviceConfig>);
static_assert(sizeof(GpuDeviceConfig) == 16);
static_assert(alignof(GpuDeviceConfig) == 4);

static_assert(std::is_standard_layout_v<ControlHeader>);
static_assert(sizeof(ControlHeader) == 24);
static_assert(alignof(ControlHeader) == 8);

static_assert(std::is_standard_layout_v<EmptyCommand>);
static_assert(sizeof(EmptyCommand) == 24);
static_assert(alignof(EmptyCommand) == 8);

static_assert(std::is_standard_layout_v<EmptyResponse>);
static_assert(sizeof(EmptyResponse) == 24);
static_assert(alignof(EmptyResponse) == 8);

static_assert(std::is_standard_layout_v<ScanoutGeometry>);
static_assert(sizeof(ScanoutGeometry) == 16);
static_assert(alignof(ScanoutGeometry) == 4);

static_assert(std::is_standard_layout_v<ScanoutInfo>);
static_assert(sizeof(ScanoutInfo) == 24);
static_assert(alignof(ScanoutInfo) == 4);

static_assert(std::is_standard_layout_v<DisplayInfoResponse>);
static_assert(sizeof(DisplayInfoResponse) == size_t{24} * 17);
static_assert(alignof(DisplayInfoResponse) == 8);

static_assert(std::is_standard_layout_v<Create2DResourceCommand>);
static_assert(sizeof(Create2DResourceCommand) == 40);
static_assert(alignof(Create2DResourceCommand) == 8);

static_assert(std::is_standard_layout_v<SetScanoutCommand>);
static_assert(sizeof(SetScanoutCommand) == 48);
static_assert(alignof(SetScanoutCommand) == 8);

static_assert(std::is_standard_layout_v<FlushResourceCommand>);
static_assert(sizeof(FlushResourceCommand) == 48);
static_assert(alignof(FlushResourceCommand) == 8);

static_assert(std::is_standard_layout_v<Transfer2DResourceToHostCommand>);
static_assert(sizeof(Transfer2DResourceToHostCommand) == 56);
static_assert(alignof(Transfer2DResourceToHostCommand) == 8);

static_assert(std::is_standard_layout_v<MemoryEntry>);
static_assert(sizeof(MemoryEntry) == 16);
static_assert(alignof(MemoryEntry) == 8);

static_assert(std::is_standard_layout_v<AttachResourceBackingCommand<1>>);
static_assert(sizeof(AttachResourceBackingCommand<1>) == 48);
static_assert(alignof(AttachResourceBackingCommand<1>) == 8);
static_assert(std::is_standard_layout_v<AttachResourceBackingCommand<2>>);
static_assert(sizeof(AttachResourceBackingCommand<2>) == 64);
static_assert(alignof(AttachResourceBackingCommand<2>) == 8);

}  // namespace virtio_abi
