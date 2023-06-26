// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>

#include "test_device_ctx.h"

namespace {

zx_driver_ops_t amlogic_video_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .init = nullptr,
    .bind = amlogic_decoder::test::AmlogicTestDevice::Create,
    // .release is not critical for this driver because dedicated devhost
    // process
};

}  // namespace

ZIRCON_DRIVER(amlogic_video_test, amlogic_video_driver_ops, "zircon", "0.1");
