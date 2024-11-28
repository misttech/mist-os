// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spmi-ctl-impl.h"

#include <fidl/fuchsia.hardware.spmi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spmi/cpp/natural_ostream.h>
#include <getopt.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <zircon/status.h>

#include <filesystem>

namespace {

constexpr char kSpmiDebugServiceDir[] = "/svc/fuchsia.hardware.spmi.DebugService";

constexpr char kUsageSummary[] = R"""(
SPMI driver control.

Usage:
  spmi-ctl [-c|--controller <controller>] -t|--target <id> -a|--address <address> -r|--read <read_bytes>
  spmi-ctl [-c|--controller <controller>] -t|--target <id> -a|--address <address> -w|--write <hex_byte0> <hex_byte1>...
  spmi-ctl [-c|--controller <controller>] -t|--target <id> -p|--properties
  spmi-ctl -l|--list
  spmi-ctl -h|--help
)""";

constexpr char kUsageDetails[] = R"""(
Options:
  -c, --controller  Controller device name. If specified, must be listed before other options. If
                    left unspecified, the first device in /svc/fuchsia.hardware.spmi.DebugService is
                    used.
  -t, --target      Target ID in [0, 15]. Must be listed before the following options.
  -a, --address     Address to read or write. Must be listed before --read or --write.
  -r, --read        Reads <read_bytes> from the device.
  -w, --write       Writes <hex_byte0>, <hex_byte1>, etc to the device.
  -p, --properties  Retrieves device properties.
  -l, --list        Lists all devices available.
  -h, --help        Show list of command-line options.

Examples:

Write 0x12 (one byte) to address 0x1234 using a Long Extended Write SPMI command:
$ spmi-ctl -t 0 -a 0x1234 -w 0x12 0x34 0x56 0x78
Executing on device: /svc/fuchsia.hardware.spmi.DebugService/c3d9294786beb5a906e4dbd5fd3b596e/device

Read one byte from address 0x1234 using a Long Extended Read SPMI command:
$ spmi-ctl -t 0 -a 0x5678 -r 1
Executing on device: /svc/fuchsia.hardware.spmi.DebugService/c3d9294786beb5a906e4dbd5fd3b596e/device
fuchsia_hardware_spmi::DeviceExtendedRegisterReadLongResponse{ data = [ 219, ], }

Get properties for device named "pmic":
$ spmi-ctl -c pmic -t 0 -p
Executing on device: pmic
fuchsia_hardware_spmi::DeviceGetPropertiesResponse{ sid = 0, name = "pmic", }

List all devices:
$ spmi-ctl -l
Controllers found:
- name: pmic

)""";

template <typename T>
std::string ToString(const T& value) {
  std::ostringstream buf;
  buf << value;
  return buf.str();
}
template <typename T>
std::string FidlString(const T& value) {
  return ToString(fidl::ostream::Formatted<T>(value));
}

void ShowUsage(bool show_details) {
  std::cout << kUsageSummary;
  if (!show_details) {
    std::cout << std::endl << "Use `spmi-ctl --help` to see full help text" << std::endl;
    return;
  }
  std::cout << kUsageDetails;
}

fidl::SyncClient<fuchsia_hardware_spmi::Debug> GetControllerByName(std::string_view name) {
  for (const auto& entry : std::filesystem::directory_iterator(kSpmiDebugServiceDir)) {
    std::string path = entry.path().string() + "/device";
    zx::result connector = component::Connect<fuchsia_hardware_spmi::Debug>(path);
    if (connector.is_error()) {
      continue;
    }
    auto client = fidl::SyncClient<fuchsia_hardware_spmi::Debug>(std::move(connector.value()));
    auto result = client->GetControllerProperties();
    if (result.is_ok() && result->name().has_value() && *result->name() == name) {
      return client;
    }
  }

  return {};
}

std::vector<fidl::Response<fuchsia_hardware_spmi::Debug::GetControllerProperties>>
GetAllControllerProperties(std::string_view directory) {
  std::vector<fidl::Response<fuchsia_hardware_spmi::Debug::GetControllerProperties>> responses;
  for (const auto& entry : std::filesystem::directory_iterator(directory)) {
    std::string path = entry.path().string() + "/device";
    zx::result connector = component::Connect<fuchsia_hardware_spmi::Debug>(path);
    if (connector.is_error()) {
      continue;
    }
    auto client = fidl::SyncClient<fuchsia_hardware_spmi::Debug>(std::move(connector.value()));
    if (auto result = client->GetControllerProperties(); result.is_ok()) {
      responses.push_back(*std::move(result));
    }
  }

  return responses;
}

void PrintControllerProperties(
    const fidl::Response<fuchsia_hardware_spmi::Debug::GetControllerProperties>& properties) {
  if (properties.name().has_value()) {
    std::cout << "- name: " << *properties.name();
  } else {
    std::cout << "- no name";
  }
  std::cout << std::endl;
}

}  // namespace

fidl::SyncClient<fuchsia_hardware_spmi::Device> SpmiCtl::GetSpmiClient(std::string controller,
                                                                       uint8_t target) {
  if (!test_client_ && !std::filesystem::exists(kSpmiDebugServiceDir)) {
    std::cerr << "Folder " << kSpmiDebugServiceDir << " not found" << std::endl;
    return {};
  }

  fidl::SyncClient<fuchsia_hardware_spmi::Debug> debug;
  if (test_client_) {
    debug = *std::move(test_client_);
  } else if (controller.empty()) {
    // If controller is not specified, use the first target entry in devfs.
    for (const auto& entry : std::filesystem::directory_iterator(kSpmiDebugServiceDir)) {
      controller = entry.path().string();
      zx::result connector =
          component::Connect<fuchsia_hardware_spmi::Debug>(controller + "/device");
      if (connector.is_error()) {
        continue;
      }
      auto client = fidl::SyncClient<fuchsia_hardware_spmi::Debug>(std::move(connector.value()));
      if (auto result = client->GetControllerProperties(); result.is_ok()) {
        debug = std::move(client);
        if (result->name().has_value()) {
          controller = *result->name();
        }
        break;
      }
    }
    if (!debug.is_valid()) {
      std::cerr << "no device found in: " << kSpmiDebugServiceDir << std::endl;
      return {};
    }
  } else if (debug = GetControllerByName(controller); !debug.is_valid()) {  // Try to find by name.
    std::cerr << "No controller found with name " << controller << std::endl;
    return {};
  }

  auto [device_client, device_server] = fidl::Endpoints<fuchsia_hardware_spmi::Device>::Create();
  if (auto result = debug->ConnectTarget({target, std::move(device_server)}); result.is_error()) {
    std::cerr << "could not connect to target " << static_cast<uint32_t>(target)
              << " on controller " << controller
              << " status:" << result.error_value().FormatDescription() << std::endl;
    return {};
  }

  std::cout << "Executing on controller: " << controller << std::endl;
  return fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(device_client));
}

void SpmiCtl::ListDevices() {
  if (!std::filesystem::exists(kSpmiDebugServiceDir)) {
    std::cerr << "Folder " << kSpmiDebugServiceDir << " not found" << std::endl;
    return;
  }

  std::vector controller_properties = GetAllControllerProperties(kSpmiDebugServiceDir);

  if (controller_properties.empty()) {
    std::cerr << "No controllers found" << std::endl;
    return;
  }

  std::cout << "Controllers found: " << std::endl;

  for (const auto& properties : controller_properties) {
    PrintControllerProperties(properties);
  }
}

int SpmiCtl::Execute(int argc, char** argv) {
  std::string controller = {};
  std::optional<uint8_t> target;
  std::optional<uint16_t> address;
  optind = 0;

  while (true) {
    static struct option long_options[] = {
        {"help", no_argument, 0, 'h'},
        {"controller", required_argument, 0, 'c'},
        {"target", required_argument, 0, 't'},
        {"address", required_argument, 0, 'a'},
        {"read", required_argument, 0, 'r'},
        {"write", required_argument, 0, 'w'},
        {"properties", no_argument, 0, 'p'},
        {"list", no_argument, 0, 'l'},
        {0, 0, 0, 0},
    };

    int c = getopt_long(argc, argv, "hc:t:a:r:w:pl", long_options, 0);
    if (c == -1)
      break;

    switch (c) {
      case 'h':
        ShowUsage(true);
        return 0;

      case 'c':
        controller = optarg;
        break;

      case 't': {
        uint32_t target_id;
        if (sscanf(optarg, "%u", &target_id) != 1) {
          ShowUsage(false);
          return -1;
        }
        if (target_id >= fuchsia_hardware_spmi::kMaxTargets) {
          std::cerr << "target must be between 0 and 15 inclusive" << std::endl;
          return -1;
        }
        target = target_id;
        break;
      }

      case 'a': {
        uint32_t local_address;
        if (sscanf(optarg, "%x", &local_address) != 1) {
          ShowUsage(false);
          return -1;
        }
        if (local_address > 0xffff) {
          std::cerr << "Address failed: must be between 0 and 0xffff inclusive" << std::endl;
          return -1;
        }
        address.emplace(local_address);
      } break;

      case 'r': {
        if (!target || !address) {
          break;
        }

        int32_t read_bytes = 0;
        if (sscanf(optarg, "%d", &read_bytes) != 1) {
          ShowUsage(false);
          return -1;
        }
        if (read_bytes < 1) {
          std::cerr << "Read failed: must read at least 1 byte" << std::endl;
          return -1;
        }

        fuchsia_hardware_spmi::DeviceExtendedRegisterReadLongRequest request;
        request.address(std::move(*address));
        request.size_bytes(static_cast<uint8_t>(read_bytes));
        auto client = GetSpmiClient(controller, *target);
        if (!client.is_valid()) {
          return -1;
        }
        auto result = client->ExtendedRegisterReadLong(std::move(request));
        if (result.is_error()) {
          std::cerr << "Read failed: " << result.error_value().FormatDescription() << std::endl;
          return -1;
        }
        std::cout << FidlString(*result) << std::endl;
        return 0;
      } break;

      case 'w': {
        if (!target || !address) {
          break;
        }

        std::vector<uint8_t> write_bytes;
        int32_t write_byte = 0;
        // At least one byte must be written.
        if (sscanf(optarg, "%x", &write_byte) != 1) {
          std::cerr << "Write failed: at least one byte must be provided in hex" << std::endl;
          return -1;
        }
        write_bytes.push_back(static_cast<uint8_t>(write_byte));
        // Add the other bytes provided.
        while (optind < argc && sscanf(argv[optind++], "%x", &write_byte) == 1) {
          write_bytes.push_back(static_cast<uint8_t>(write_byte));
        }
        fuchsia_hardware_spmi::DeviceExtendedRegisterWriteLongRequest request;
        request.address(std::move(*address));
        request.data(std::move(write_bytes));
        auto client = GetSpmiClient(controller, *target);
        if (!client.is_valid()) {
          return -1;
        }
        auto result = client->ExtendedRegisterWriteLong(std::move(request));
        if (result.is_error()) {
          std::cerr << "Write failed: " << result.error_value().FormatDescription() << std::endl;
          return -1;
        }
        std::cout << "Write success" << std::endl;

        return 0;
      } break;

      case 'p': {
        if (!target) {
          break;
        }

        auto client = GetSpmiClient(controller, *target);
        if (!client.is_valid()) {
          return -1;
        }
        auto result = client->GetProperties();
        if (result.is_error()) {
          std::cerr << "Get properties failed: " << result.error_value().FormatDescription()
                    << std::endl;
          return -1;
        }
        std::cout << FidlString(*result) << std::endl;
        return 0;
      }

      case 'l':
        ListDevices();
        return 0;

      default:
        ShowUsage(false);
        return -1;
    }
  }

  ShowUsage(false);
  return -1;
}
