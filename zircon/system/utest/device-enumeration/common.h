// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_UTEST_DEVICE_ENUMERATION_COMMON_H_
#define ZIRCON_SYSTEM_UTEST_DEVICE_ENUMERATION_COMMON_H_

#include <zxtest/zxtest.h>

#include "src/lib/fsl/io/device_watcher.h"

namespace device_enumeration {

// Asynchronously wait for a path to appear, and call `callback` when the path exists.
// The `watchers` array is needed because each directory in the path needs to allocate a
// DeviceWatcher, and they need to be stored somewhere that can be freed later.
void RecursiveWaitFor(const std::string& full_path, size_t slash_index,
                      fit::function<void()> callback,
                      std::vector<std::unique_ptr<fsl::DeviceWatcher>>& watchers,
                      async_dispatcher_t* dispatcher);

void WaitForOne(cpp20::span<const char*> device_paths);

void WaitForClassDeviceCount(const std::string& path_in_devfs, size_t count);

bool IsDfv2Enabled();
}  // namespace device_enumeration

class DeviceEnumerationTest : public zxtest::Test {
  void SetUp() override { ASSERT_NO_FATAL_FAILURE(PrintAllDevices()); }

 protected:
  static void TestRunner(const char** device_paths, size_t paths_num);

 private:
  static void PrintAllDevices();
};

#endif  // ZIRCON_SYSTEM_UTEST_DEVICE_ENUMERATION_COMMON_H_
