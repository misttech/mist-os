// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/channel.h>
#include <magma_intel_gen_defs.h>

#include <gtest/gtest.h>

#include "helper/test_device_helper.h"
#include "magma/magma.h"
#include "magma_util/short_macros.h"

namespace {

TEST(Intel, IcdList) {
  magma::TestDeviceBase test_device(MAGMA_VENDOR_ID_INTEL);

  // TODO(https://fxbug.dev/112484): This relies on multiplexing.
  fidl::UnownedClientEnd<fuchsia_gpu_magma::IcdLoaderDevice> channel{
      test_device.channel().channel()->borrow()};
  const fidl::WireResult result = fidl::WireCall(channel)->GetIcdList();
  EXPECT_TRUE(result.ok()) << result.FormatDescription();
  const fidl::WireResponse response = result.value();
  EXPECT_EQ(response.icd_list.count(), 3u);
  auto& icd_item = response.icd_list[0];
  EXPECT_TRUE(icd_item.has_flags());
  EXPECT_TRUE(icd_item.flags() & fuchsia_gpu_magma::wire::IcdFlags::kSupportsVulkan);
  std::string res_string(icd_item.component_url().get());
  EXPECT_EQ(res_string.length(), icd_item.component_url().size());
  EXPECT_EQ(std::string("fuchsia-pkg://fuchsia.com/libvulkan_intel_gen_test#meta/vulkan.cm"),
            res_string);
}

}  // namespace
