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

constexpr char kSpmiTargetServiceDir[] = "/svc/fuchsia.hardware.spmi.TargetService";
constexpr char kSpmiSubTargetServiceDir[] = "/svc/fuchsia.hardware.spmi.SubTargetService";

constexpr char kUsageSummary[] = R"""(
SPMI driver control.

Usage:
  spmi-ctl [-t|--target <device>] -a|--address <address> -r|--read <read_bytes>
  spmi-ctl [-t|--target <device>] -a|--address <address> -w|--write <hex_byte0> <hex_byte1>...
  spmi-ctl [-t|--target <device>] -p|--properties
  spmi-ctl -s|--sub-target <device> -a|--address <address> -r|--read <read_bytes>
  spmi-ctl -s|--sub-target <device> -a|--address <address> -w|--write <hex_byte0> <hex_byte1>...
  spmi-ctl -s|--sub-target <device> -p|--properties
  spmi-ctl -l|--list
  spmi-ctl -h|--help
)""";

constexpr char kUsageDetails[] = R"""(
Options:
  -t, --target      Target device name or path. The name can be specified e.g. pmic,
                    the full service path can be specified e.g.
                    /svc/fuchsia.hardware.spmi.TargetService/3afc81c75f364aafed716537adbb5b9a/device,
                    the service name can be specified e.g. 3afc81c75f364aafed716537adbb5b9a/device,
                    or it can be left unspecified (picks the first device in
                    /svc/fuchsia.hardware.spmi.TargetService).
                    If specified, must be listed before other options.
  -s, --sub-target  Sub-target device name or path. The name can be specified e.g. pmic,
                    the full service path can be specified e.g.
                    /svc/fuchsia.hardware.spmi.SubTargetService/3afc81c75f364aafed716537adbb5b9a/device,
                    the service name can be specified e.g. 3afc81c75f364aafed716537adbb5b9a/device.
                    Must be listed before other options.
  -a, --address     Address to read or write. Must be listed before --read or --write.
  -r, --read        Reads <read_bytes> from the device.
  -w, --write       Writes <hex_byte0>, <hex_byte1>, etc to the device.
  -p, --properties  Retrieves device properties.
  -l, --list        Lists all devices available.
  -h, --help        Show list of command-line options.

Examples:

Write 0x12 (one byte) to address 0x1234 using a Long Extended Write SPMI command:
$ spmi-ctl -a 0x1234 -w 0x12 0x34 0x56 0x78
Executing on device: /svc/fuchsia.hardware.spmi.TargetService/c3d9294786beb5a906e4dbd5fd3b596e/device

Read one byte from address 0x1234 using a Long Extended Read SPMI command:
$ spmi-ctl -a 0x5678 -r 1
Executing on device: /svc/fuchsia.hardware.spmi.TargetService/c3d9294786beb5a906e4dbd5fd3b596e/device
fuchsia_hardware_spmi::DeviceExtendedRegisterReadLongResponse{ data = [ 219, ], }

Get properties for device named "pmic":
$ spmi-ctl -t pmic -p
Executing on device: pmic
fuchsia_hardware_spmi::DeviceGetPropertiesResponse{ sid = 0, name = "pmic", }

List all devices:
$ spmi-ctl -l
Devices found:
- sid: 0 name: pmic
Sub-targets found:
- sid: 1 name: gpio
- sid: 1 name: i2c

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

fidl::SyncClient<fuchsia_hardware_spmi::Device> GetDeviceByName(std::string_view name,
                                                                std::string_view directory) {
  for (const auto& entry : std::filesystem::directory_iterator(directory)) {
    std::string path = entry.path().string() + "/device";
    zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(path);
    if (connector.is_error()) {
      continue;
    }
    auto client = fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
    auto result = client->GetProperties();
    if (result.is_ok() && result->name().has_value() && *result->name() == name) {
      return client;
    }
  }

  return {};
}

std::vector<fidl::Response<fuchsia_hardware_spmi::Device::GetProperties>> GetAllDeviceProperties(
    std::string_view directory) {
  std::vector<fidl::Response<fuchsia_hardware_spmi::Device::GetProperties>> responses;
  for (const auto& entry : std::filesystem::directory_iterator(directory)) {
    std::string path = entry.path().string() + "/device";
    zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(path);
    if (connector.is_error()) {
      continue;
    }
    auto client = fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
    if (auto result = client->GetProperties(); result.is_ok()) {
      responses.push_back(*std::move(result));
    }
  }

  return responses;
}

void PrintDeviceProperties(
    const fidl::Response<fuchsia_hardware_spmi::Device::GetProperties>& properties) {
  if (properties.sid().has_value()) {
    std::cout << "- sid: " << std::to_string(*properties.sid());
    if (properties.name().has_value()) {
      std::cout << " name: " << *properties.name();
    }
    std::cout << std::endl;
  }
}

}  // namespace

fidl::SyncClient<fuchsia_hardware_spmi::Device> SpmiCtl::GetSpmiClient(std::string target,
                                                                       std::string sub_target) {
  if (test_client_) {
    return *std::move(test_client_);
  }
  if (!std::filesystem::exists(kSpmiTargetServiceDir)) {
    std::cerr << "Folder " << kSpmiTargetServiceDir << " not found" << std::endl;
    return {};
  }
  if (!std::filesystem::exists(kSpmiSubTargetServiceDir)) {
    std::cerr << "Folder " << kSpmiSubTargetServiceDir << " not found" << std::endl;
    return {};
  }

  if (!target.empty() && !sub_target.empty()) {
    std::cerr << "target and sub-target options are mutually exclusive" << std::endl;
    ShowUsage(false);
    return {};
  }

  std::string device;

  // Try to find by name.
  if (!target.empty()) {
    fidl::SyncClient<fuchsia_hardware_spmi::Device> client =
        GetDeviceByName(target, kSpmiTargetServiceDir);
    if (client.is_valid()) {
      std::cout << "Executing on target: " << target << std::endl;
      return client;
    }

    // If target is a hex number (and hence we have not found a full path above),
    // for instance "1234abcd" make it "/svc/fuchsia.hardware.spmi.TargetService/1234abcd".
    int node_number = -1;
    if (sscanf(target.c_str(), "%x", &node_number) == 1) {
      target = std::string(kSpmiTargetServiceDir) + "/" + target;
    }

    device = target + "/device";
    std::cout << "Executing on target: " << device << std::endl;
  } else if (!sub_target.empty()) {
    fidl::SyncClient<fuchsia_hardware_spmi::Device> client =
        GetDeviceByName(target, kSpmiSubTargetServiceDir);
    if (client.is_valid()) {
      std::cout << "Executing on sub-target: " << sub_target << std::endl;
      return client;
    }

    int node_number = -1;
    if (sscanf(sub_target.c_str(), "%x", &node_number) == 1) {
      sub_target = std::string(kSpmiSubTargetServiceDir) + "/" + sub_target;
    }

    device = sub_target + "/device";
    std::cout << "Executing on sub-target: " << device << std::endl;
  } else {
    // If neither target nor sub-target are specified, use the first target entry in devfs.
    for (const auto& entry : std::filesystem::directory_iterator(kSpmiTargetServiceDir)) {
      target = entry.path().string();
      break;
    }
    if (!target.size()) {
      std::cerr << "no device found in: " << kSpmiTargetServiceDir << std::endl;
      return {};
    }

    device = target + "/device";
    std::cout << "Executing on target: " << device << std::endl;
  }

  zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(device.c_str());
  if (connector.is_error()) {
    std::cerr << "could not connect to:" << device << " status:" << connector.status_string()
              << std::endl;
    return {};
  }
  return fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
}

void SpmiCtl::ListDevices() {
  if (!std::filesystem::exists(kSpmiTargetServiceDir)) {
    std::cerr << "Folder " << kSpmiTargetServiceDir << " not found" << std::endl;
    return;
  }
  if (!std::filesystem::exists(kSpmiSubTargetServiceDir)) {
    std::cerr << "Folder " << kSpmiSubTargetServiceDir << " not found" << std::endl;
    return;
  }

  std::vector target_properties = GetAllDeviceProperties(kSpmiTargetServiceDir);
  std::vector sub_target_properties = GetAllDeviceProperties(kSpmiSubTargetServiceDir);

  if (target_properties.empty() && sub_target_properties.empty()) {
    std::cerr << "No devices found" << std::endl;
    return;
  }

  if (!target_properties.empty()) {
    std::cout << "Targets found: " << std::endl;
  }

  for (const auto& properties : target_properties) {
    PrintDeviceProperties(properties);
  }

  if (!target_properties.empty()) {
    std::cout << "Sub-targets found: " << std::endl;
  }

  for (const auto& properties : sub_target_properties) {
    PrintDeviceProperties(properties);
  }
}

int SpmiCtl::Execute(int argc, char** argv) {
  std::string target = {};
  std::string sub_target = {};
  std::optional<uint16_t> address;
  optind = 0;

  while (true) {
    static struct option long_options[] = {
        {"help", no_argument, 0, 'h'},
        {"target", required_argument, 0, 't'},
        {"sub-target", required_argument, 0, 's'},
        {"address", required_argument, 0, 'a'},
        {"read", required_argument, 0, 'r'},
        {"write", required_argument, 0, 'w'},
        {"properties", no_argument, 0, 'p'},
        {"list", no_argument, 0, 'l'},
        {0, 0, 0, 0},
    };

    int c = getopt_long(argc, argv, "ht:s:a:r:w:pl", long_options, 0);
    if (c == -1)
      break;

    switch (c) {
      case 'h':
        ShowUsage(true);
        return 0;

      case 't':
        target = optarg;
        break;

      case 's':
        sub_target = optarg;
        break;

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
        if (!address) {
          break;
        }

        int32_t read_bytes = 0;
        if (sscanf(optarg, "%d", &read_bytes) != 1) {
          ShowUsage(false);
          return -1;
        }
        if (read_bytes < 1 || read_bytes > 8) {
          std::cerr << "Read failed: read_bytes must be between 1 and 8 inclusive" << std::endl;
          return -1;
        }

        fuchsia_hardware_spmi::DeviceExtendedRegisterReadLongRequest request;
        request.address(std::move(*address));
        request.size_bytes(static_cast<uint8_t>(read_bytes));
        auto client = GetSpmiClient(target, sub_target);
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
        if (!address) {
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
        auto client = GetSpmiClient(target, sub_target);
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
        auto client = GetSpmiClient(target, sub_target);
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
