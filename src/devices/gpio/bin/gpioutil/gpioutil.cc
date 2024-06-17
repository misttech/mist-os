// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpioutil.h"

#include <dirent.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <zircon/types.h>

#include <filesystem>
#include <optional>
#include <string_view>

// Directory path to the GPIO class
#define GPIO_DEV_CLASS_PATH "/dev/class/gpio/"
constexpr char kGpioDevClassDir[] = GPIO_DEV_CLASS_PATH;
constexpr char kGpioDevClassNsDir[] = "/ns" GPIO_DEV_CLASS_PATH;

constexpr std::optional<fuchsia_hardware_gpio::wire::GpioFlags> ParseInputFlag(
    std::string_view arg) {
  if (arg == "down") {
    return fuchsia_hardware_gpio::wire::GpioFlags::kPullDown;
  }
  if (arg == "up") {
    return fuchsia_hardware_gpio::wire::GpioFlags::kPullUp;
  }
  if (arg == "none") {
    return fuchsia_hardware_gpio::wire::GpioFlags::kNoPull;
  }
  return {};
}

constexpr std::optional<uint32_t> ParseInterruptFlags(std::string_view arg) {
  if (arg == "default") {
    return ZX_INTERRUPT_MODE_DEFAULT;
  }
  if (arg == "edge-low") {
    return ZX_INTERRUPT_MODE_EDGE_LOW;
  }
  if (arg == "edge-high") {
    return ZX_INTERRUPT_MODE_EDGE_HIGH;
  }
  if (arg == "edge-both") {
    return ZX_INTERRUPT_MODE_EDGE_BOTH;
  }
  if (arg == "level-low") {
    return ZX_INTERRUPT_MODE_LEVEL_LOW;
  }
  if (arg == "level-high") {
    return ZX_INTERRUPT_MODE_LEVEL_HIGH;
  }
  return {};
}

int ParseArgs(int argc, char** argv, GpioFunc* func, uint8_t* write_value,
              fuchsia_hardware_gpio::wire::GpioFlags* in_flag, uint8_t* out_value, uint64_t* ds_ua,
              uint32_t* interrupt_flags) {
  if (argc < 2) {
    return -1;
  }

  std::string_view func_arg = argv[1];

  /* Following functions allow no args */
  switch (func_arg[0]) {
    case 'l':
      *func = List;
      return 0;
  }

  if (argc < 3) {
    return -1;
  }

  *write_value = 0;
  *in_flag = fuchsia_hardware_gpio::wire::GpioFlags::kNoPull;
  *out_value = 0;
  *ds_ua = 0;
  *interrupt_flags = 0;
  switch (func_arg[0]) {
    case 'n':
      *func = GetName;
      break;
    case 'r':
      *func = Read;
      break;
    case 'w':
      *func = Write;

      if (argc < 4) {
        return -1;
      }
      *write_value = static_cast<uint8_t>(std::stoul(argv[3]));
      break;
    case 'i':
    case 'q': {
      if (argc < 4) {
        return -1;
      }

      if (func_arg[0] == 'q' || func_arg == "interrupt") {
        *func = Interrupt;
        std::optional<uint32_t> parsed_interrupt_flags = ParseInterruptFlags(argv[3]);
        if (!parsed_interrupt_flags) {
          fprintf(stderr, "Invalid interrupt flag \"%s\"\n\n", argv[3]);
          return -1;
        }

        *interrupt_flags = *parsed_interrupt_flags;
        break;
      }

      *func = ConfigIn;

      std::optional<fuchsia_hardware_gpio::wire::GpioFlags> parsed_in_flag =
          ParseInputFlag(argv[3]);
      if (!parsed_in_flag) {
        fprintf(stderr, "Invalid flag \"%s\"\n\n", argv[3]);
        return -1;
      }
      *in_flag = *parsed_in_flag;
      break;
    }
    case 'o':
      *func = ConfigOut;

      if (argc < 4) {
        return -1;
      }
      *out_value = static_cast<uint8_t>(std::stoul(argv[3]));
      break;
    case 'd':
      if (argc >= 4) {
        *func = SetDriveStrength;
        *ds_ua = static_cast<uint64_t>(std::stoull(argv[3]));
      } else if (argc == 3) {
        *func = GetDriveStrength;
      } else {
        return -1;
      }

      break;
    default:
      *func = Invalid;
      return -1;
  }

  return 0;
}

zx::result<> ListGpios() {
  const char* dev_class_dir = kGpioDevClassNsDir;
  if (!std::filesystem::is_directory(dev_class_dir)) {
    dev_class_dir = kGpioDevClassDir;
  }

  for (auto const& dir_entry : std::filesystem::directory_iterator(dev_class_dir)) {
    const char* gpio_path = dir_entry.path().c_str();
    zx::result client_end = component::Connect<fuchsia_hardware_gpio::Gpio>(gpio_path);
    if (client_end.is_error()) {
      fprintf(stderr, "Could not connect to client from %s: %s\n", gpio_path,
              client_end.status_string());
      return client_end.take_error();
    }

    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> client(std::move(client_end.value()));
    const fidl::WireResult result_pin = client->GetPin();
    if (!result_pin.ok()) {
      fprintf(stderr, "Could not get pin from %s: %s\n", gpio_path,
              result_pin.FormatDescription().c_str());
      return zx::error(result_pin.status());
    }
    const fit::result response_pin = result_pin.value();
    if (response_pin.is_error()) {
      fprintf(stderr, "Could not get pin from %s: %s\n", gpio_path,
              zx_status_get_string(response_pin.error_value()));
      return zx::error(result_pin.status());
    }
    const fidl::WireResult result_name = client->GetName();
    if (!result_name.ok()) {
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path,
              result_name.FormatDescription().c_str());
      return zx::error(result_name.status());
    }
    const fit::result response_name = result_name.value();
    if (response_name.is_error()) {
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path,
              zx_status_get_string(response_name.error_value()));
      return zx::error(result_name.status());
    }

    uint32_t pin = response_pin.value()->pin;
    std::string_view name = response_name.value()->name.get();
    printf("[gpio-%d] %.*s\n", pin, static_cast<int>(name.length()), name.data());
  }

  return zx::ok();
}

zx::result<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> FindGpioClientByName(
    std::string_view name) {
  const char* dev_class_dir = kGpioDevClassNsDir;
  if (!std::filesystem::is_directory(dev_class_dir)) {
    dev_class_dir = kGpioDevClassDir;
  }

  for (auto const& dir_entry : std::filesystem::directory_iterator(dev_class_dir)) {
    const char* gpio_path = dir_entry.path().c_str();

    zx::result client_end = component::Connect<fuchsia_hardware_gpio::Gpio>(gpio_path);
    if (client_end.is_error()) {
      fprintf(stderr, "Could not connect to client from %s: %s\n", gpio_path,
              client_end.status_string());
      return client_end.take_error();
    }

    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> client(std::move(client_end.value()));

    const fidl::WireResult result_name = client->GetName();
    if (!result_name.ok()) {
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path,
              result_name.FormatDescription().c_str());
      return zx::error(result_name.status());
    }
    const fit::result response_name = result_name.value();
    if (response_name.is_error()) {
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path,
              zx_status_get_string(response_name.error_value()));
      return zx::error(result_name.status());
    }
    std::string_view gpio_name = response_name.value()->name.get();
    if (name == gpio_name) {
      return zx::ok(std::move(client));
    }
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

int ClientCall(fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> client, GpioFunc func,
               uint8_t write_value, fuchsia_hardware_gpio::wire::GpioFlags in_flag,
               uint8_t out_value, uint64_t ds_ua, uint32_t interrupt_flags) {
  switch (func) {
    case GetName: {
      auto result_pin = client->GetPin();
      if (!result_pin.ok()) {
        fprintf(stderr, "Call to get Pin failed: %s\n", result_pin.FormatDescription().c_str());
        return -2;
      }
      if (result_pin->is_error()) {
        fprintf(stderr, "Failed to get Pin: %s\n", zx_status_get_string(result_pin->error_value()));
        return -2;
      }
      auto result_name = client->GetName();
      if (!result_name.ok()) {
        fprintf(stderr, "Call to get Name failed: %s\n", result_name.FormatDescription().c_str());
        return -2;
      }
      if (result_name->is_error()) {
        fprintf(stderr, "Failed to get Name: %s\n",
                zx_status_get_string(result_name->error_value()));
        return -2;
      }
      auto pin = result_pin->value()->pin;
      auto name = result_name->value()->name.get();
      printf("GPIO Name: [gpio-%d] %.*s\n", pin, static_cast<int>(name.length()), name.data());
      break;
    }
    case Read: {
      auto result = client->Read();
      if (!result.ok()) {
        fprintf(stderr, "Call to read GPIO failed: %s\n", result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not read GPIO: %s\n", zx_status_get_string(result->error_value()));
        return -2;
      }
      printf("GPIO Value: %u\n", result->value()->value);
      break;
    }
    case Write: {
      auto result = client->Write(write_value);
      if (!result.ok()) {
        fprintf(stderr, "Call to write GPIO failed: %s\n", result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not write GPIO: %s\n", zx_status_get_string(result->error_value()));
        return -2;
      }
      break;
    }
    case ConfigIn: {
      auto result = client->ConfigIn(in_flag);
      if (!result.ok()) {
        fprintf(stderr, "Call to configure GPIO as input failed: %s\n",
                result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not configure GPIO as input: %s\n",
                zx_status_get_string(result->error_value()));
        return -2;
      }
      break;
    }
    case ConfigOut: {
      auto result = client->ConfigOut(out_value);
      if (!result.ok()) {
        fprintf(stderr, "Call to configure GPIO as output failed: %s\n",
                result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not configure GPIO as output: %s\n",
                zx_status_get_string(result->error_value()));
        return -2;
      }
      break;
    }
    case SetDriveStrength: {
      auto result = client->SetDriveStrength(ds_ua);
      if (!result.ok()) {
        fprintf(stderr, "Call to set GPIO drive strength failed: %s\n",
                result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not set GPIO drive strength: %s\n",
                zx_status_get_string(result->error_value()));
        return -2;
      }
      printf("Set drive strength to %lu\n", result->value()->actual_ds_ua);
      break;
    }
    case GetDriveStrength: {
      auto result = client->GetDriveStrength();
      if (!result.ok()) {
        fprintf(stderr, "Call to get GPIO drive strength failed: %s\n",
                result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not get GPIO drive strength: %s\n",
                zx_status_get_string(result->error_value()));
        return -2;
      }
      printf("Drive Strength: %lu ua\n", result->value()->result_ua);
      break;
    }
    case Interrupt: {
      auto result = client->GetInterrupt(interrupt_flags);
      if (!result.ok()) {
        fprintf(stderr, "Call to get GPIO interrupt failed: %s\n",
                result.FormatDescription().c_str());
        return -2;
      }
      if (result->is_error()) {
        fprintf(stderr, "Could not get GPIO interrupt: %s\n",
                zx_status_get_string(result->error_value()));
        return -2;
      }

      zx::time timestamp{};
      if (zx_status_t status = result->value()->irq.wait(&timestamp); status != ZX_OK) {
        fprintf(stderr, "Interrupt wait failed: %s\n", zx_status_get_string(status));
        return -2;
      }

      printf("Received interrupt at time %ld\n", timestamp.get());

      auto release = client->ReleaseInterrupt();
      if (!release.ok()) {
        fprintf(stderr, "Call to release GPIO interrupt failed: %s\n",
                release.FormatDescription().c_str());
        return -2;
      }
      if (release->is_error()) {
        fprintf(stderr, "Could not release GPIO interrupt: %s\n",
                zx_status_get_string(release->error_value()));
        return -2;
      }
      break;
    }
    default:
      fprintf(stderr, "Invalid function\n\n");
      return -1;
  }
  return 0;
}
