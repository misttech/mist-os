// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

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

}  // namespace device_enumeration

void DeviceEnumerationTest::VerifyNodes(cpp20::span<const char*> node_monikers) {
  std::vector<std::string> missing_nodes;
  for (const char* moniker : node_monikers) {
    if (node_info_.find(moniker) == node_info_.end()) {
      missing_nodes.push_back(moniker);
    }
  }

  if (!missing_nodes.empty()) {
    fprintf(stderr, "Unable to find node(s):\n");
    for (auto& moniker : missing_nodes) {
      fprintf(stderr, "     %s:\n", moniker.c_str());
    }
  }

  ASSERT_TRUE(missing_nodes.empty());
}

void DeviceEnumerationTest::VerifyOneOf(cpp20::span<const char*> node_monikers) {
  std::vector<std::string> missing_nodes;
  bool found_node = false;
  for (const char* moniker : node_monikers) {
    if (node_info_.find(moniker) != node_info_.end()) {
      found_node = true;
      break;
    }
  }

  if (!found_node) {
    fprintf(stderr, "Unable to find one of the node(s):\n");
    for (const char* moniker : node_monikers) {
      fprintf(stderr, "     %s:\n", moniker);
    }
  }
  ASSERT_TRUE(found_node);
}

void DeviceEnumerationTest::RetrieveNodeInfo() {
  // This uses the development API for its convenience over directory traversal. It would be more
  // useful to log paths in devfs for the purposes of this test, but less convenient.
  zx::result driver_development = component::Connect<fuchsia_driver_development::Manager>();
  ASSERT_OK(driver_development.status_value());

  const fidl::Status bootup_result = fidl::WireCall(driver_development.value())->WaitForBootup();
  ASSERT_OK(bootup_result.status());

  {
    auto [client, server] = fidl::Endpoints<fuchsia_driver_development::NodeInfoIterator>::Create();

    const fidl::Status result = fidl::WireCall(driver_development.value())
                                    ->GetNodeInfo({}, std::move(server), /* exact_match= */ true);
    ASSERT_OK(result.status());

    // NB: this uses iostream (rather than printf) because FIDL strings aren't null-terminated.
    std::cout << "BEGIN printing all node monikers:" << std::endl;
    while (true) {
      const fidl::WireResult result = fidl::WireCall(client)->GetNext();
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      if (response.nodes.empty()) {
        break;
      }
      for (const fuchsia_driver_development::wire::NodeInfo& info : response.nodes) {
        ASSERT_TRUE(info.has_moniker());
        if (info.has_quarantined() && info.quarantined()) {
          std::cerr << info.moniker().get() << " exists but has failed to start successfully."
                    << std::endl;
        } else {
          std::cout << info.moniker().get() << std::endl;
          node_info_[std::string(info.moniker().get())] = fidl::ToNatural(info);
        }
      }
    }
    std::cout << "END printing all node monikers." << std::endl;
  }
}
