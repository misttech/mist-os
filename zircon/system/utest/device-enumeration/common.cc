// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <iostream>
#include <map>
#include <unordered_set>

#include <fbl/string_printf.h>
#include <fbl/vector.h>

namespace device_enumeration {

void RecursiveWaitFor(const std::string& full_path, size_t slash_index,
                      fit::function<void()> callback,
                      std::vector<std::unique_ptr<fsl::DeviceWatcher>>& watchers,
                      async_dispatcher_t* dispatcher) {
  if (slash_index == full_path.size()) {
    fprintf(stderr, "Found %s \n", full_path.c_str());
    callback();
    return;
  }

  const std::string dir_path = full_path.substr(0, slash_index);
  size_t next_slash = full_path.find('/', slash_index + 1);
  if (next_slash == std::string::npos) {
    next_slash = full_path.size();
  }
  const std::string file_name = full_path.substr(slash_index + 1, next_slash - (slash_index + 1));

  watchers.push_back(fsl::DeviceWatcher::Create(
      dir_path,
      [file_name, full_path, next_slash, callback = std::move(callback), &watchers, dispatcher](
          const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& name) mutable {
        if (name == file_name) {
          RecursiveWaitFor(full_path, next_slash, std::move(callback), watchers, dispatcher);
        }
      },
      dispatcher));
}

void WaitForOne(cpp20::span<const char*> device_paths) {
  async::Loop loop = async::Loop(&kAsyncLoopConfigNeverAttachToThread);

  async::TaskClosure task([device_paths]() {
    // stdout doesn't show up in test logs.
    fprintf(stderr, "still waiting for device paths:\n");
    for (const char* path : device_paths) {
      fprintf(stderr, " %s\n", path);
    }
  });
  ASSERT_OK(task.PostDelayed(loop.dispatcher(), zx::min(1)));

  std::vector<std::unique_ptr<fsl::DeviceWatcher>> watchers;
  for (const char* path : device_paths) {
    RecursiveWaitFor(
        std::string("/dev/") + path, 4, [&loop]() { loop.Shutdown(); }, watchers,
        loop.dispatcher());
  }

  loop.Run();
}

void WaitForClassDeviceCount(const std::string& path_in_devfs, size_t count) {
  async::Loop loop = async::Loop(&kAsyncLoopConfigNeverAttachToThread);

  async::TaskClosure task([path_in_devfs, &count]() {
    // stdout doesn't show up in test logs.
    fprintf(stderr, "still waiting for %zu devices in %s\n", count, path_in_devfs.c_str());
  });

  ASSERT_OK(task.PostDelayed(loop.dispatcher(), zx::min(1)));

  std::map<std::string, int> devices_found;

  std::unique_ptr watcher = fsl::DeviceWatcher::Create(
      std::string("/dev/") + path_in_devfs,
      [&devices_found, &count, &loop](const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                      const std::string& name) {
        devices_found.emplace(name, 0);
        if (devices_found.size() == count) {
          loop.Shutdown();
        }
      },
      loop.dispatcher());

  loop.Run();
}

bool IsDfv2Enabled() {
  zx::result driver_development =
      component::Connect<fuchsia_driver_development::DriverDevelopment>();
  EXPECT_OK(driver_development.status_value());

  const fidl::WireResult result = fidl::WireCall(driver_development.value())->IsDfv2();
  EXPECT_OK(result.status());
  return result.value().response;
}

}  // namespace device_enumeration

// Static
void DeviceEnumerationTest::TestRunner(const char** device_paths, size_t paths_num) {
  async::Loop loop = async::Loop(&kAsyncLoopConfigNeverAttachToThread);

  std::unordered_set<const char*> device_paths_set;
  for (size_t i = 0; i < paths_num; ++i) {
    device_paths_set.emplace(device_paths[i]);
  }
  async::TaskClosure task;
  std::vector<std::unique_ptr<fsl::DeviceWatcher>> watchers;
  {
    // Intentionally shadow.
    std::unordered_set<const char*>& device_paths = device_paths_set;
    task.set_handler([&device_paths]() {
      // stdout doesn't show up in test logs.
      fprintf(stderr, "still waiting for device paths:\n");
      for (const char* path : device_paths) {
        fprintf(stderr, " %s\n", path);
      }
    });
    ASSERT_OK(task.PostDelayed(loop.dispatcher(), zx::min(1)));

    for (const char* path : device_paths) {
      device_enumeration::RecursiveWaitFor(
          std::string("/dev/") + path, 4,
          [&loop, &device_paths, path]() {
            ASSERT_EQ(device_paths.erase(path), 1);
            if (device_paths.empty()) {
              loop.Shutdown();
            }
          },
          watchers, loop.dispatcher());
    }
  }
  loop.Run();
}

// Static
void DeviceEnumerationTest::PrintAllDevices() {
  // This uses the development API for its convenience over directory traversal. It would be more
  // useful to log paths in devfs for the purposes of this test, but less convenient.
  zx::result driver_development =
      component::Connect<fuchsia_driver_development::DriverDevelopment>();
  ASSERT_OK(driver_development.status_value());

  const fidl::WireResult result = fidl::WireCall(driver_development.value())->IsDfv2();
  ASSERT_OK(result.status());
  const bool is_dfv2 = result.value().response;

  {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_development::DeviceInfoIterator>();
    ASSERT_OK(endpoints.status_value());
    auto& [client, server] = endpoints.value();

    const fidl::Status result = fidl::WireCall(driver_development.value())
                                    ->GetDeviceInfo({}, std::move(server), /* exact_match= */ true);
    ASSERT_OK(result.status());

    // NB: this uses iostream (rather than printf) because FIDL strings aren't null-terminated.
    std::cout << "BEGIN printing all devices (paths in DFv1, monikers in DFv2):" << std::endl;
    while (true) {
      const fidl::WireResult result = fidl::WireCall(client)->GetNext();
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      if (response.drivers.empty()) {
        break;
      }
      for (const fuchsia_driver_development::wire::DeviceInfo& info : response.drivers) {
        if (is_dfv2) {
          ASSERT_TRUE(info.has_moniker());
          std::cout << info.moniker().get() << std::endl;
        } else {
          ASSERT_TRUE(info.has_topological_path());
          std::cout << info.topological_path().get() << std::endl;
        }
      }
    }
    std::cout << "END printing all devices (paths in DFv1, monikers in DFv2)." << std::endl;
  }
}
