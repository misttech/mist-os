// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fidl/fuchsia.hardware.usb.peripheral/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/usb-peripheral-utils/event-watcher.h>

#include <filesystem>

#include <usb/cdc.h>
#include <usb/peripheral.h>
#include <usb/usb.h>

constexpr char kDevUsbPeripheralDir[] = "/dev/class/usb-peripheral";

#define MANUFACTURER_STRING "Zircon"
#define CDC_PRODUCT_STRING "CDC Ethernet"
#define RNDIS_PRODUCT_STRING "RNDIS Ethernet"
#define UMS_PRODUCT_STRING "USB Mass Storage"
#define TEST_PRODUCT_STRING "USB Function Test"
#define CDC_TEST_PRODUCT_STRING "CDC Ethernet & USB Function Test"
#define SERIAL_STRING "12345678"

namespace peripheral = fuchsia_hardware_usb_peripheral;

const peripheral::wire::FunctionDescriptor cdc_function_descs[] = {
    {
        .interface_class = USB_CLASS_COMM,
        .interface_subclass = USB_CDC_SUBCLASS_ETHERNET,
        .interface_protocol = 0,
    },
};

const peripheral::wire::FunctionDescriptor ums_function_descs[] = {
    {
        .interface_class = USB_CLASS_MSC,
        .interface_subclass = USB_SUBCLASS_MSC_SCSI,
        .interface_protocol = USB_PROTOCOL_MSC_BULK_ONLY,
    },
};

const peripheral::wire::FunctionDescriptor rndis_function_descs[] = {
    {
        .interface_class = USB_CLASS_MISC,
        .interface_subclass = USB_SUBCLASS_MSC_RNDIS,
        .interface_protocol = USB_PROTOCOL_MSC_RNDIS_ETHERNET,
    },
};

const peripheral::wire::FunctionDescriptor test_function_descs[] = {
    {
        .interface_class = USB_CLASS_VENDOR,
        .interface_subclass = 0,
        .interface_protocol = 0,
    },
};

const peripheral::wire::FunctionDescriptor cdc_test_function_descs[] = {
    {
        .interface_class = USB_CLASS_COMM,
        .interface_subclass = USB_CDC_SUBCLASS_ETHERNET,
        .interface_protocol = 0,
    },
    {
        .interface_class = USB_CLASS_VENDOR,
        .interface_subclass = 0,
        .interface_protocol = 0,
    },
};

using usb_config_t = struct {
  const peripheral::wire::FunctionDescriptor* descs;
  size_t descs_count;
  const char* product_string;
  uint16_t vid;
  uint16_t pid;
};

namespace {

const usb_config_t cdc_function = {
    .descs = cdc_function_descs,
    .descs_count = std::size(cdc_function_descs),
    .product_string = CDC_PRODUCT_STRING,
    .vid = GOOGLE_USB_VID,
    .pid = GOOGLE_USB_CDC_PID,
};

const usb_config_t ums_function = {
    .descs = ums_function_descs,
    .descs_count = std::size(ums_function_descs),
    .product_string = UMS_PRODUCT_STRING,
    .vid = GOOGLE_USB_VID,
    .pid = GOOGLE_USB_UMS_PID,
};

const usb_config_t rndis_function = {
    .descs = rndis_function_descs,
    .descs_count = std::size(rndis_function_descs),
    .product_string = RNDIS_PRODUCT_STRING,
    .vid = GOOGLE_USB_VID,
    .pid = GOOGLE_USB_RNDIS_PID,
};

const usb_config_t test_function = {
    .descs = test_function_descs,
    .descs_count = std::size(test_function_descs),
    .product_string = TEST_PRODUCT_STRING,
    .vid = GOOGLE_USB_VID,
    .pid = GOOGLE_USB_FUNCTION_TEST_PID,
};

const usb_config_t cdc_test_function = {
    .descs = cdc_test_function_descs,
    .descs_count = std::size(cdc_test_function_descs),
    .product_string = CDC_TEST_PRODUCT_STRING,
    .vid = GOOGLE_USB_VID,
    .pid = GOOGLE_USB_CDC_AND_FUNCTION_TEST_PID,
};

peripheral::wire::DeviceDescriptor device_desc = {
    .bcd_usb = htole16(0x0200),
    .b_device_class = 0,
    .b_device_sub_class = 0,
    .b_device_protocol = 0,
    .b_max_packet_size0 = 64,
    //   idVendor and idProduct are filled in later
    .bcd_device = htole16(0x0100),
    //    iManufacturer, iProduct and iSerialNumber are filled in later
    .b_num_configurations = 1,
};

zx_status_t device_init(const fidl::WireSyncClient<peripheral::Device>& client,
                        const usb_config_t* config) {
  using ConfigurationDescriptor =
      ::fidl::VectorView<fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor>;
  device_desc.id_vendor = htole16(config->vid);
  device_desc.id_product = htole16(config->pid);
  device_desc.manufacturer = MANUFACTURER_STRING;
  device_desc.product = fidl::StringView::FromExternal(config->product_string);
  device_desc.serial = SERIAL_STRING;
  ConfigurationDescriptor config_desc;
  peripheral::wire::FunctionDescriptor func_descs[config->descs_count];
  memcpy(func_descs, config->descs,
         sizeof(peripheral::wire::FunctionDescriptor) * config->descs_count);
  config_desc = ConfigurationDescriptor::FromExternal(func_descs, config->descs_count);
  auto resp = client->SetConfiguration(
      device_desc, fidl::VectorView<ConfigurationDescriptor>::FromExternal(&config_desc, 1));
  if (resp.status() != ZX_OK) {
    return resp.status();
  }
  if (resp->is_error()) {
    return resp->error_value();
  }
  return ZX_OK;
}

zx_status_t device_clear_functions(const fidl::WireSyncClient<peripheral::Device>& client) {
  zx::result endpoints = fidl::CreateEndpoints<peripheral::Events>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }
  auto& [client_end, server_end] = endpoints.value();
  auto set_result = client->SetStateChangeListener(std::move(client_end));
  if (set_result.status() != ZX_OK) {
    return set_result.status();
  }

  auto clear_functions = client->ClearFunctions();
  if (clear_functions.status() != ZX_OK) {
    return clear_functions.status();
  }

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  usb_peripheral_utils::EventWatcher watcher(loop, std::move(server_end), 0);
  loop.Run();
  if (!watcher.all_functions_cleared()) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

int ums_command(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                const char* argv[]) {
  zx_status_t status = device_clear_functions(client);
  if (status == ZX_OK) {
    status = device_init(client, &ums_function);
  }

  return status == ZX_OK ? 0 : -1;
}

int cdc_command(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                const char* argv[]) {
  zx_status_t status = device_clear_functions(client);
  if (status == ZX_OK) {
    status = device_init(client, &cdc_function);
  }

  return status == ZX_OK ? 0 : -1;
}

int rndis_command(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                  const char* argv[]) {
  zx_status_t status = device_clear_functions(client);
  if (status == ZX_OK) {
    status = device_init(client, &rndis_function);
  }

  return status == ZX_OK ? 0 : -1;
}

int test_command(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                 const char* argv[]) {
  zx_status_t status = device_clear_functions(client);
  if (status == ZX_OK) {
    status = device_init(client, &test_function);
  }

  return status == ZX_OK ? 0 : -1;
}

int cdc_test_command(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                     const char* argv[]) {
  zx_status_t status = device_clear_functions(client);
  if (status == ZX_OK) {
    status = device_init(client, &cdc_test_function);
  }

  return status == ZX_OK ? 0 : -1;
}

using usbctl_command_t = struct {
  const char* name;
  int (*command)(const fidl::WireSyncClient<peripheral::Device>& client, int argc,
                 const char* argv[]);
  const char* description;
};

usbctl_command_t commands[] = {
    {"init-ums", ums_command, "init-ums - initializes the USB Mass Storage function"},
    {"init-cdc", cdc_command, "init-cdc - initializes the CDC Ethernet function"},
    {"init-rndis", rndis_command, "init-rndis - initializes the RNDIS Ethernet function"},
    {"init-test", test_command, "init-test - initializes the USB Peripheral Test function"},
    {"init-cdc-test", cdc_test_command,
     "init-cdc-test - initializes CDC plus Test Function composite device"},
    {nullptr, nullptr, nullptr},
};

void usage() {
  fprintf(stderr, "usage: \"usbctl <command>\", where command is one of:\n");

  usbctl_command_t* command = commands;
  while (command->name) {
    fprintf(stderr, "    %s\n", command->description);
    command++;
  }
}

}  // namespace

int main(int argc, const char** argv) {
  if (argc < 2) {
    usage();
    return -1;
  }

  std::optional<std::string> path;
  for (const auto& dir_entry : std::filesystem::directory_iterator(kDevUsbPeripheralDir)) {
    path = dir_entry.path().string();
    break;
  }
  if (!path.has_value()) {
    fprintf(stderr, "could not find a device in %s\n", kDevUsbPeripheralDir);
    return -1;
  }
  zx::result client_end = component::Connect<peripheral::Device>(path.value());
  if (client_end.is_error()) {
    fprintf(stderr, "could not connect to device %s: %s\n", path.value().c_str(),
            client_end.status_string());
    return -1;
  }
  const fidl::WireSyncClient client{std::move(client_end.value())};

  const char* command_name = argv[1];
  usbctl_command_t* command = commands;
  while (command->name) {
    if (!strcmp(command_name, command->name)) {
      if (zx_status_t status = command->command(client, argc - 1, argv + 1); status != ZX_OK) {
        fprintf(stderr, "command '%s' failed: %s\n", command->name, zx_status_get_string(status));
        return -1;
      };
      return 0;
    }
    command++;
  }
  // if we fall through, print usage
  usage();
  return -1;
}
