// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_RANGE_H_
#define LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_RANGE_H_

#include "sdk/lib/driver/mock-mmio/cpp/globally-ordered-region.h"

// TODO(b/373899421): Remove this header once all usages are migrated.
namespace mock_mmio {

using MockMmioRange = GloballyOrderedRegion;

}  // namespace mock_mmio

#endif  // LIB_DRIVER_MOCK_MMIO_CPP_MOCK_MMIO_RANGE_H_
