// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.adc/cpp/wire.h>
#include <fidl/fuchsia.hardware.temperature/cpp/wire.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <stdio.h>
#include <string.h>

constexpr char kUsageMessage[] = R"""(Usage: temperature-cli <device> <command>

    resolution - Get adc resolution (for adc class device)
    read - read adc sample (for adc class device)
    readnorm - read normalized adc sample [0.0-1.0] (for adc class device)
    trippoint - Get/set trippoint. Follow with multiple sets of index:type,configuration.
                No spaces allowed.
                index must be an integer.
                type is allowed to be "above", "below".
                configuration can be a float, interpreted as temperature in celsius, or "cleared".
    wait - Wait for a trippoint to be triggered
    name - Get sensor name (for temperature class device)

    Example:
    temperature-cli /dev/class/temperature/000
    temperature-cli /dev/class/temperature/000 name
    - or -
    temperature-cli /dev/class/adc/000 read
    temperature-cli /dev/class/adc/000 resolution
    - or -
    temperature-cli /dev/class/trippoint/000 trippoint
    temperature-cli /dev/class/trippoint/000 trippoint 0:below,4.2 1:above,50.1 2:above,cleared
    temperature-cli /dev/class/trippoint/000 wait
    temperature-cli /dev/class/trippoint/000 name
)""";

namespace FidlTemperature = fuchsia_hardware_temperature;
namespace FidlAdc = fuchsia_hardware_adc;
namespace FidlTrippoint = fuchsia_hardware_trippoint;

std::string ToString(const FidlTrippoint::wire::TripPointType& type) {
  switch (type) {
    case fuchsia_hardware_trippoint::TripPointType::kOneshotTempAbove:
      return "OneshotTempAbove";
    case fuchsia_hardware_trippoint::TripPointType::kOneshotTempBelow:
      return "OneshotTempBelow";
    default:
      return "Unknown";
  }
}

std::string ToString(const FidlTrippoint::wire::TripPointValue& value) {
  switch (value.Which()) {
    case fuchsia_hardware_trippoint::wire::TripPointValue::Tag::kClearedTripPoint:
      return "ClearedTripPoint";
    case fuchsia_hardware_trippoint::wire::TripPointValue::Tag::kOneshotTempAboveTripPoint:
      return "OneshotTempAboveTripPoint(" +
             std::to_string(value.oneshot_temp_above_trip_point().critical_temperature_celsius) +
             ")";
    case fuchsia_hardware_trippoint::wire::TripPointValue::Tag::kOneshotTempBelowTripPoint:
      return "OneshotTempBelowTripPoint(" +
             std::to_string(value.oneshot_temp_below_trip_point().critical_temperature_celsius) +
             ")";
    default:
      return "Unknown";
  }
}

void print_trippoint(
    const fidl::VectorView<FidlTrippoint::wire::TripPointDescriptor>& descriptors) {
  for (const auto& trippoint : descriptors) {
    printf("{\n");
    printf("   .index = %d,\n", trippoint.index);
    printf("   .type = %s,\n", ToString(trippoint.type).c_str());
    printf("   .configuration = %s,\n", ToString(trippoint.configuration).c_str());
    printf("},\n");
  }
}

std::vector<FidlTrippoint::wire::TripPointDescriptor> parse_set_trippoints_args(int argc,
                                                                                char** argv) {
  assert(argc > 3);
  std::vector<FidlTrippoint::wire::TripPointDescriptor> descriptors;
  for (int i = 3; i < argc; i++) {
    // Parse string
    std::string trippoint = argv[i];
    auto delim = trippoint.find(':');
    if (delim == std::string::npos) {
      return {};
    }
    std::string index = trippoint.substr(0, delim);
    trippoint.erase(0, delim + 1);
    delim = trippoint.find(',');
    if (delim == std::string::npos) {
      return {};
    }
    std::string type = trippoint.substr(0, delim);
    trippoint.erase(0, delim + 1);
    std::string configuration = trippoint;

    constexpr std::string kCleared = "cleared";
    FidlTrippoint::wire::TripPointDescriptor desc;
    if (type == "above") {
      desc = {
          .type = FidlTrippoint::wire::TripPointType::kOneshotTempAbove,
          .index = static_cast<uint32_t>(atoi(index.data())),
          .configuration = configuration == kCleared
                               ? FidlTrippoint::wire::TripPointValue::WithClearedTripPoint(
                                     FidlTrippoint::wire::ClearedTripPoint())
                               : FidlTrippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(
                                     FidlTrippoint::wire::OneshotTempAboveTripPoint(
                                         static_cast<float>(atof(configuration.data()))))};
    } else if (type == "below") {
      desc = {
          .type = FidlTrippoint::wire::TripPointType::kOneshotTempBelow,
          .index = static_cast<uint32_t>(atoi(index.data())),
          .configuration = configuration == kCleared
                               ? FidlTrippoint::wire::TripPointValue::WithClearedTripPoint(
                                     FidlTrippoint::wire::ClearedTripPoint())
                               : FidlTrippoint::wire::TripPointValue::WithOneshotTempBelowTripPoint(
                                     FidlTrippoint::wire::OneshotTempBelowTripPoint(
                                         static_cast<float>(atof(configuration.data()))))};
    } else {
      printf("%s", kUsageMessage);
      return {};
    }

    descriptors.emplace_back(desc);
  }
  return descriptors;
}

int main(int argc, char** argv) {
  if (argc < 2) {
    printf("%s", kUsageMessage);
    return 0;
  }

  zx::channel local, remote;
  zx_status_t status = zx::channel::create(0, &local, &remote);
  if (status != ZX_OK) {
    printf("Failed to create channel: status = %d\n", status);
    return -1;
  }

  status = fdio_service_connect(argv[1], remote.release());
  if (status != ZX_OK) {
    printf("Failed to open sensor: status = %d\n", status);
    return -1;
  }

  if (argc < 3) {
    fidl::WireSyncClient<FidlTemperature::Device> client(
        fidl::ClientEnd<FidlTemperature::Device>(std::move(local)));

    auto response = client->GetTemperatureCelsius();
    if (response.ok()) {
      if (!response->status) {
        printf("temperature = %f\n", response->temp);
      } else {
        printf("GetTemperatureCelsius failed: status = %d\n", response->status);
      }
    }
  } else {
    if (strcmp(argv[2], "resolution") == 0) {
      fidl::WireSyncClient<FidlAdc::Device> client(
          fidl::ClientEnd<FidlAdc::Device>(std::move(local)));
      auto response = client->GetResolution();
      if (response.ok()) {
        if (response->is_error()) {
          printf("GetResolution failed: status = %d\n", response->error_value());
        } else {
          printf("adc resolution  = %u\n", response->value()->resolution);
        }
      } else {
        printf("GetResolution fidl call failed: status = %d\n", response.status());
      }
    } else if (strcmp(argv[2], "read") == 0) {
      fidl::WireSyncClient<FidlAdc::Device> client(
          fidl::ClientEnd<FidlAdc::Device>(std::move(local)));
      auto response = client->GetSample();
      if (response.ok()) {
        if (response->is_error()) {
          printf("GetSample failed: status = %d\n", response->error_value());
        } else {
          printf("Value = %u\n", response->value()->value);
        }
      } else {
        printf("GetSample fidl call failed: status = %d\n", response.status());
      }
    } else if (strcmp(argv[2], "readnorm") == 0) {
      fidl::WireSyncClient<FidlAdc::Device> client(
          fidl::ClientEnd<FidlAdc::Device>(std::move(local)));
      auto response = client->GetNormalizedSample();
      if (response.ok()) {
        if (response->is_error()) {
          printf("GetSampleNormalized failed: status = %d\n", response->error_value());
        } else {
          printf("Value  = %f\n", response->value()->value);
        }
      } else {
        printf("GetSampleNormalized fidl call failed: status = %d\n", response.status());
      }
    } else if (strcmp(argv[2], "trippoint") == 0) {
      fidl::WireSyncClient<FidlTrippoint::TripPoint> client(
          fidl::ClientEnd<FidlTrippoint::TripPoint>(std::move(local)));
      if (argc == 3) {
        auto response = client->GetTripPointDescriptors();
        if (response.ok()) {
          if (response->is_error()) {
            printf("GetTripPointDescriptors failed: status = %d\n", response->error_value());
          } else {
            print_trippoint(response->value()->descriptors);
          }
        } else {
          printf("GetTripPointDescriptors fidl call failed: status = %d\n", response.status());
        }
      } else {
        std::vector descriptors = parse_set_trippoints_args(argc, argv);
        if (descriptors.empty()) {
          printf("Invalid trippoints list\n");
          return 1;
        }
        auto fidl_descriptors =
            fidl::VectorView<FidlTrippoint::wire::TripPointDescriptor>::FromExternal(descriptors);
        printf("Setting trippoints:\n");
        print_trippoint(fidl_descriptors);
        auto response = client->SetTripPoints(fidl_descriptors);
        if (response.ok()) {
          if (response->is_error()) {
            printf("SetTripPoints failed: status = %d\n", response->error_value());
          }
        } else {
          printf("SetTripPoints fidl call failed: status = %d\n", response.status());
        }
      }
    } else if (strcmp(argv[2], "wait") == 0) {
      fidl::WireSyncClient<FidlTrippoint::TripPoint> client(
          fidl::ClientEnd<FidlTrippoint::TripPoint>(std::move(local)));
      auto response = client->WaitForAnyTripPoint();
      if (response.ok()) {
        if (response->is_error()) {
          printf("WaitForAnyTripPoint failed: status = %d\n", response->error_value());
        } else {
          printf("TripPoint indexed %d was tripped. Measured temperature was %f C\n",
                 response->value()->result.index,
                 response->value()->result.measured_temperature_celsius);
        }
      } else {
        printf("WaitForAnyTripPoint fidl call failed: status = %d\n", response.status());
      }
    } else if (strcmp(argv[2], "name") == 0) {
      fidl::WireSyncClient<FidlTemperature::Device> client(
          fidl::ClientEnd<FidlTemperature::Device>(std::move(local)));

      auto response = client->GetSensorName();
      if (response.ok()) {
        printf("Sensor Name = %s\n",
               std::string(response->name.data(), response->name.size()).c_str());
      } else {
        printf("GetSensorName fidl call failed: status = %d\n", response.status());
      }
    } else {
      printf("%s", kUsageMessage);
      return 1;
    }
  }
  return 0;
}
