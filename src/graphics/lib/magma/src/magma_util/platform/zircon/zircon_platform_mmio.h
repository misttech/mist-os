// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mmio/mmio-buffer.h>
#include <lib/mmio/mmio-pinned-buffer.h>

#include "magma_util/dlog.h"
#include "magma_util/short_macros.h"
#include "platform_mmio.h"

namespace magma {

class ZirconPlatformMmio : public PlatformMmio {
 public:
  explicit ZirconPlatformMmio(fdf::MmioBuffer mmio);

  ~ZirconPlatformMmio();
  bool Pin(const zx::bti& bti);
  uint64_t physical_address() override;

 private:
  fdf::MmioBuffer mmio_;
  std::optional<fdf::MmioPinnedBuffer> pinned_mmio_;
};

}  // namespace magma
