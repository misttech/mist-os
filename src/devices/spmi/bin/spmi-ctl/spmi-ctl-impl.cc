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

constexpr char kSpmiServiceDir[] = "/svc/fuchsia.hardware.spmi.Service";

constexpr char kUsageSummary[] = R"""(
SPMI driver control.

Usage:
  spmi-ctl [-d|--device <device>] -a|--address <address> -r|--read <read_bytes>
  spmi-ctl [-d|--device <device>] -a|--address <address> -w|--write <hex_byte0> <hex_byte1>...
  spmi-ctl [-d|--device <device>] -p|--properties
  spmi-ctl -l|--list
  spmi-ctl -h|--help
)""";

constexpr char kUsageDetails[] = R"""(
Options:
  -d, --device      Device name or path. The name can be specified e.g. pmic,
                    the full service path can be specified e.g.
                    /svc/fuchsia.hardware.spmi.Service/3afc81c75f364aafed716537adbb5b9a/device,
                    the service name can be specified e.g. 3afc81c75f364aafed716537adbb5b9a/device,
                    or it can be left unspecified (picks the first device in
                    /svc/fuchsia.hardware.spmi.Service).
                    If specified, must be listed before other options.
  -a, --address     Address to read or write. Must be listed before --read or --write.
  -r, --read        Reads <read_bytes> from the device.
  -w, --write       Writes <hex_byte0>, <hex_byte1>, etc to the device.
  -p, --properties  Retrieves device properties.
  -l, --list        Lists all devices available.
  -h, --help        Show list of command-line options.

Examples:

List all available devices:
$ spmi-ctl -l
Devices found:
- pmic

Write 0x12 (one byte) to address 0x1234 using a Long Extended Write SPMI command:
$ spmi-ctl -a 0x1234 -w 0x12 0x34 0x56 0x78
Executing on device: /svc/fuchsia.hardware.spmi.Service/c3d9294786beb5a906e4dbd5fd3b596e/device

Read one byte from address 0x1234 using a Long Extended Read SPMI command:
$ spmi-ctl -a 0x5678 -r 1
Executing on device: /svc/fuchsia.hardware.spmi.Service/c3d9294786beb5a906e4dbd5fd3b596e/device
fuchsia_hardware_spmi::DeviceExtendedRegisterReadLongResponse{ data = [ 219, ], }

Get properties for device named "pmic":
$ spmi-ctl -d pmic -p
Executing on device: pmic
fuchsia_hardware_spmi::DeviceGetPropertiesResponse{ sid = 0, name = "pmic", }

List all devices:
$ spmi-ctl -l
Devices found:
- sid: 0 name: pmic

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

}  // namespace

fidl::SyncClient<fuchsia_hardware_spmi::Device> SpmiCtl::GetSpmiClient(std::string device) {
  if (test_client_) {
    return *std::move(test_client_);
  }
  if (!std::filesystem::exists(kSpmiServiceDir)) {
    std::cerr << "Folder " << kSpmiServiceDir << " not found" << std::endl;
    return {};
  }

  // Try to find by name.
  if (!device.empty()) {
    for (const auto& entry : std::filesystem::directory_iterator(kSpmiServiceDir)) {
      std::string path = entry.path().string() + "/device";
      zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(path.c_str());
      if (connector.is_error()) {
        continue;
      }
      auto client = fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
      auto result = client->GetProperties();
      if (result.is_ok() && result->name().has_value() && *result->name() == device) {
        std::cout << "Executing on device: " << device << std::endl;
        return client;
      }
    }
  }

  // If not specified, used the first entry in devfs.
  if (device.empty()) {
    for (const auto& entry : std::filesystem::directory_iterator(kSpmiServiceDir)) {
      device = entry.path().string();
      break;
    }
    if (!device.size()) {
      std::cerr << "no device found in: " << kSpmiServiceDir << std::endl;
      return {};
    }
  }

  // If device is a hex number (and hence we have not found a full path above),
  // for instance "1234abcd" make it "/svc/fuchsia.hardware.spmi.Service/1234abcd".
  int node_number = -1;
  if (sscanf(device.c_str(), "%x", &node_number) == 1) {
    device = std::string(kSpmiServiceDir) + "/" + device;
  }

  device = device + "/device";

  std::cout << "Executing on device: " << device << std::endl;
  zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(device.c_str());
  if (connector.is_error()) {
    std::cerr << "could not connect to:" << device << " status:" << connector.status_string()
              << std::endl;
    return {};
  }
  return fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
}

void SpmiCtl::ListDevices() {
  if (!std::filesystem::exists(kSpmiServiceDir)) {
    std::cerr << "Folder " << kSpmiServiceDir << " not found" << std::endl;
    return;
  }
  bool found = false;
  for (const auto& entry : std::filesystem::directory_iterator(kSpmiServiceDir)) {
    std::string path = entry.path().string() + "/device";
    zx::result connector = component::Connect<fuchsia_hardware_spmi::Device>(path.c_str());
    if (connector.is_error()) {
      continue;
    }
    auto client = fidl::SyncClient<fuchsia_hardware_spmi::Device>(std::move(connector.value()));
    auto result = client->GetProperties();
    if (result.is_ok() && result->sid().has_value()) {
      if (!found) {
        std::cout << "Devices found: " << std::endl;
        found = true;
      }
      std::cout << "- sid: " << std::to_string(*result->sid());
      if (result->name().has_value()) {
        std::cout << " name: " << *result->name();
      }
      std::cout << std::endl;
    }
  }
  if (!found) {
    std::cerr << "No devices found" << std::endl;
  }
}

int SpmiCtl::Execute(int argc, char** argv) {
  std::string device = {};
  std::optional<uint16_t> address;
  optind = 0;

  while (true) {
    static struct option long_options[] = {
        {"help", no_argument, 0, 'h'},          {"device", required_argument, 0, 'd'},
        {"address", required_argument, 0, 'a'}, {"read", required_argument, 0, 'r'},
        {"write", required_argument, 0, 'w'},   {"properties", no_argument, 0, 'p'},
        {"list", no_argument, 0, 'l'},          {0, 0, 0, 0}};

    int c = getopt_long(argc, argv, "hd:a:r:w:pl", long_options, 0);
    if (c == -1)
      break;

    switch (c) {
      case 'h':
        ShowUsage(true);
        return 0;

      case 'd':
        device = optarg;
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
        auto client = GetSpmiClient(device);
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
        auto client = GetSpmiClient(device);
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
        auto client = GetSpmiClient(device);
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
