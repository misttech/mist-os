// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEFINITIONS_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEFINITIONS_H_

#include <fuchsia/hardware/network/driver/c/banjo.h>
#include <zircon/types.h>

#include <array>

#include "src/lib/vmo_store/vmo_store.h"

namespace network {
#if __mist_os__
constexpr uint16_t kMaxFifoDepth = (PAGE_SIZE / 2) / sizeof(uint16_t);
#else
constexpr uint16_t kMaxFifoDepth = PAGE_SIZE / sizeof(uint16_t);
#endif

namespace internal {
template <typename T>
using BufferParts = std::array<T, MAX_BUFFER_PARTS>;
using DataVmoStore = vmo_store::VmoStore<vmo_store::SlabStorage<uint8_t>>;
}  // namespace internal

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEFINITIONS_H_
