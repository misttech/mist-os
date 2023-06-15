// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpioutil.h"

#include <dirent.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <stdio.h>

#include <filesystem>

// Directory path to the GPIO class
#define GPIO_DEV_CLASS_PATH "/dev/class/gpio/"
constexpr char kGpioDevClassDir[] = GPIO_DEV_CLASS_PATH;
constexpr char kGpioDevClassNsDir[] = "/ns" GPIO_DEV_CLASS_PATH;

int ParseArgs(int argc, char** argv, GpioFunc* func, uint8_t* write_value,
              fuchsia_hardware_gpio::wire::GpioFlags* in_flag, uint8_t* out_value,
              uint64_t* ds_ua) {
  if (argc < 2) {
    return -1;
  }

  /* Following functions allow no args */
  switch (argv[1][0]) {
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
  unsigned long flag = 0;
  switch (argv[1][0]) {
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
      *func = ConfigIn;

      if (argc < 4) {
        return -1;
      }
      flag = std::stoul(argv[3]);
      if (flag > 3) {
        fprintf(stderr, "Invalid flag\n\n");
        return -1;
      }
      *in_flag = static_cast<fuchsia_hardware_gpio::wire::GpioFlags>(flag);
      break;
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
      fprintf(stderr, "Could not get pin from %s: %s\n", gpio_path, result_pin.status_string());
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
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path, result_name.status_string());
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
      fprintf(stderr, "Could not get name from %s: %s\n", gpio_path, result_name.status_string());
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
               uint8_t out_value, uint64_t ds_ua) {
  switch (func) {
    case GetName: {
      auto result_pin = client->GetPin();
      if (!result_pin.ok()) {
        fprintf(stderr, "Could not get Pin\n");
        return -2;
      }
      auto result_name = client->GetName();
      if (!result_name.ok()) {
        fprintf(stderr, "Could not get Name\n");
        return -2;
      }
      auto pin = result_pin->value()->pin;
      auto name = result_name->value()->name.get();
      printf("GPIO Name: [gpio-%d] %.*s\n", pin, static_cast<int>(name.length()), name.data());
      break;
    }
    case Read: {
      auto result = client->Read();
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not read GPIO\n");
        return -2;
      }
      printf("GPIO Value: %u\n", result->value()->value);
      break;
    }
    case Write: {
      auto result = client->Write(write_value);
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not write to GPIO\n");
        return -2;
      }
      break;
    }
    case ConfigIn: {
      auto result = client->ConfigIn(in_flag);
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not configure GPIO as input\n");
        return -2;
      }
      break;
    }
    case ConfigOut: {
      auto result = client->ConfigOut(out_value);
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not configure GPIO as output\n");
        return -2;
      }
      break;
    }
    case SetDriveStrength: {
      auto result = client->SetDriveStrength(ds_ua);
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not set GPIO drive strength\n");
        return -2;
      }
      printf("Set drive strength to %lu\n", result->value()->actual_ds_ua);
      break;
    }
    case GetDriveStrength: {
      auto result = client->GetDriveStrength();
      if ((result.status() != ZX_OK) || result->is_error()) {
        fprintf(stderr, "Could not get drive strength\n");
        return -2;
      }
      printf("Drive Strength: %lu ua\n", result->value()->result_ua);
      break;
    }
    default:
      fprintf(stderr, "Invalid function\n\n");
      return -1;
  }
  return 0;
}
