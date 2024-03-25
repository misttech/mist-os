// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_hci_broadcom.h"

#include <assert.h>
#include <endian.h>
#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/random.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/threads.h>

namespace bt_hci_broadcom {

constexpr uint32_t kTargetBaudRate = 2000000;
constexpr uint32_t kDefaultBaudRate = 115200;

constexpr zx::duration kFirmwareDownloadDelay = zx::msec(50);

// Hardcoded. Better to parameterize on chipset. Broadcom chips need a few hundred msec delay after
// firmware load.
constexpr zx::duration kBaudRateSwitchDelay = zx::msec(200);

const std::unordered_map<uint16_t, std::string> BtHciBroadcom::kFirmwareMap = {
    {PDEV_PID_BCM43458, "BCM4345C5.hcd"},
    {PDEV_PID_BCM4359, "BCM4359C0.hcd"},
};

BtHciBroadcom::BtHciBroadcom(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("bt-hci-broadcom", std::move(start_args), std::move(driver_dispatcher)),
      node_(fidl::WireClient(std::move(node()), dispatcher())),
      devfs_connector_(fit::bind_member<&BtHciBroadcom::Connect>(this)) {}

void BtHciBroadcom::Start(fdf::StartCompleter completer) {
  zx_status_t status = ConnectToHciFidlProtocol();
  if (status != ZX_OK) {
    completer(zx::error(status));
    return;
  }

  status = ConnectToSerialFidlProtocol();
  if (status == ZX_OK) {
    is_uart_ = true;
  }

  fdf::Arena arena('INFO');
  auto result = serial_client_.buffer(arena)->GetInfo();
  if (!result.ok()) {
    FDF_LOG(ERROR, "Read failed FIDL error: %s", result.status_string());
    completer(zx::error(result.status()));
    return;
  }

  if (result->is_error()) {
    FDF_LOG(ERROR, "Read failed : %s", zx_status_get_string(result->error_value()));
    completer(zx::error(result->error_value()));
    return;
  }

  serial_pid_ = result.value()->info.serial_pid;

  // Continue initialization through the fpromise executor.
  start_completer_.emplace(std::move(completer));
  executor_.emplace(dispatcher());
  executor_->schedule_task(Initialize().then([this](fpromise::result<void, zx_status_t>& result) {
    if (result.is_ok()) {
      CompleteStart(ZX_OK);
    } else {
      CompleteStart(result.take_error());
    }
  }));
}

void BtHciBroadcom::PrepareStop(fdf::PrepareStopCompleter completer) {
  command_channel_.reset();
  completer(zx::ok());
}

void BtHciBroadcom::GetFeatures(GetFeaturesCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_bluetooth::BtVendorFeatures::kSetAclPriorityCommand);
}

void BtHciBroadcom::EncodeCommand(EncodeCommandRequestView request,
                                  EncodeCommandCompleter::Sync& completer) {
  uint8_t data_buffer[kBcmSetAclPriorityCmdSize];
  switch (request->command.Which()) {
    case fuchsia_hardware_bluetooth::wire::BtVendorCommand::Tag::kSetAclPriority: {
      EncodeSetAclPriorityCommand(request->command.set_acl_priority(), data_buffer);
      auto encoded_cmd =
          fidl::VectorView<uint8_t>::FromExternal(data_buffer, kBcmSetAclPriorityCmdSize);
      completer.ReplySuccess(encoded_cmd);
      return;
    }
    default: {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
  }
}

void BtHciBroadcom::OpenHci(OpenHciCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }

  hci_server_bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                  std::move(endpoints->server), this, fidl::kIgnoreBindingClosure);
  completer.ReplySuccess(std::move(endpoints->client));
}

void BtHciBroadcom::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Vendor protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void BtHciBroadcom::OpenCommandChannel(OpenCommandChannelRequestView request,
                                       OpenCommandChannelCompleter::Sync& completer) {
  hci_client_->OpenCommandChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenCommandChannel>&
                  result) mutable {
            if (!result.ok()) {
              FDF_LOG(ERROR, "OpenCommandChannel failed with FIDL error %s",
                      result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              FDF_LOG(ERROR, "OpenCommandChannel failed with error %s",
                      zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}

void BtHciBroadcom::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                       OpenAclDataChannelCompleter::Sync& completer) {
  hci_client_->OpenAclDataChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenAclDataChannel>&
                  result) mutable {
            if (!result.ok()) {
              FDF_LOG(ERROR, "OpenAclDataChannel failed with FIDL error %s",
                      result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              FDF_LOG(ERROR, "OpenAclDataChannel failed with error %s",
                      zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}
void BtHciBroadcom::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                       OpenScoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::ConfigureSco(ConfigureScoRequestView request,
                                 ConfigureScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::ResetSco(ResetScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                       OpenIsoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtHciBroadcom::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                     OpenSnoopChannelCompleter::Sync& completer) {
  hci_client_->OpenSnoopChannel(std::move(request->channel))
      .ThenExactlyOnce(
          [completer = completer.ToAsync()](
              fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Hci::OpenSnoopChannel>&
                  result) mutable {
            if (!result.ok()) {
              FDF_LOG(ERROR, "OpenSnoopChannel failed with FIDL error %s", result.status_string());
              completer.ReplyError(result.status());
              return;
            }
            if (result->is_error()) {
              FDF_LOG(ERROR, "OpenSnoopChannel failed with error %s",
                      zx_status_get_string(result->error_value()));
              completer.ReplyError(result->error_value());
              return;
            }
            completer.ReplySuccess();
          });
}

void BtHciBroadcom::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Hci protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

// driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor>
void BtHciBroadcom::Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request) {
  vendor_binding_group_.AddBinding(dispatcher(), std::move(request), this,
                                   fidl::kIgnoreBindingClosure);
}

zx_status_t BtHciBroadcom::ConnectToHciFidlProtocol() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci>> client_end =
      incoming()->Connect<fuchsia_hardware_bluetooth::HciService::Hci>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to fuchsia_hardware_bluetooth::Hci protocol failed: %s",
            client_end.status_string());
    return client_end.status_value();
  }

  hci_client_ =
      fidl::WireClient(*std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  return ZX_OK;
}

zx_status_t BtHciBroadcom::ConnectToSerialFidlProtocol() {
  zx::result<fdf::ClientEnd<fuchsia_hardware_serialimpl::Device>> client_end =
      incoming()->Connect<fuchsia_hardware_serialimpl::Service::Device>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to fuchsia_hardware_serialimpl::Device protocol failed: %s",
            client_end.status_string());
    return client_end.status_value();
  }

  serial_client_ = fdf::WireSyncClient(*std::move(client_end));
  return ZX_OK;
}

void BtHciBroadcom::EncodeSetAclPriorityCommand(
    fuchsia_hardware_bluetooth::wire::BtVendorSetAclPriorityParams params, void* out_buffer) {
  BcmSetAclPriorityCmd command = {
      .header =
          {
              .opcode = htole16(kBcmSetAclPriorityCmdOpCode),
              .parameter_total_size = sizeof(BcmSetAclPriorityCmd) - sizeof(HciCommandHeader),
          },
      .connection_handle = htole16(params.connection_handle),
      .priority = (params.priority == fuchsia_hardware_bluetooth::BtVendorAclPriority::kNormal)
                      ? kBcmAclPriorityNormal
                      : kBcmAclPriorityHigh,
      .direction = (params.direction == fuchsia_hardware_bluetooth::BtVendorAclDirection::kSource)
                       ? kBcmAclDirectionSource
                       : kBcmAclDirectionSink,
  };

  memcpy(out_buffer, &command, sizeof(command));
}

fpromise::promise<std::vector<uint8_t>, zx_status_t> BtHciBroadcom::SendCommand(const void* command,
                                                                                size_t length) {
  // send HCI command
  zx_status_t status = command_channel_.write(/*flags=*/0, command, static_cast<uint32_t>(length),
                                              /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "command channel write failed %s", zx_status_get_string(status));
    return fpromise::make_result_promise<std::vector<uint8_t>, zx_status_t>(
        fpromise::error(status));
  }

  return ReadEvent();
}

fpromise::promise<std::vector<uint8_t>, zx_status_t> BtHciBroadcom::ReadEvent() {
  return executor_
      ->MakePromiseWaitHandle(zx::unowned_handle(command_channel_.get()),
                              ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED)
      .then([this](fpromise::result<zx_packet_signal_t, zx_status_t>&)
                -> fpromise::result<std::vector<uint8_t>, zx_status_t> {
        std::vector<uint8_t> read_buf(kChanReadBufLen, 0u);
        uint32_t actual = 0;
        zx_status_t status = command_channel_.read(
            /*flags=*/0, read_buf.data(), /*handles=*/nullptr, kChanReadBufLen,
            /*num_handles=*/0, &actual, /*actual_handles=*/nullptr);
        if (status != ZX_OK) {
          return fpromise::error(status);
        }

        if (actual < sizeof(HciCommandComplete)) {
          FDF_LOG(ERROR, "command channel read too short: %d < %lu", actual,
                  sizeof(HciCommandComplete));
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        HciCommandComplete event;
        std::memcpy(&event, read_buf.data(), sizeof(HciCommandComplete));
        if (event.header.event_code != kHciEvtCommandCompleteEventCode ||
            event.header.parameter_total_size < kMinEvtParamSize) {
          FDF_LOG(ERROR, "did not receive command complete or params too small");
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        if (event.return_code != 0) {
          FDF_LOG(ERROR, "got command complete error %u", event.return_code);
          return fpromise::error(ZX_ERR_INTERNAL);
        }

        read_buf.resize(actual);
        return fpromise::ok(std::move(read_buf));
      });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SetBaudRate(uint32_t baud_rate) {
  BcmSetBaudRateCmd command = {
      .header =
          {
              .opcode = kBcmSetBaudRateCmdOpCode,
              .parameter_total_size = sizeof(BcmSetBaudRateCmd) - sizeof(HciCommandHeader),
          },
      .unused = 0,
      .baud_rate = htole32(baud_rate),
  };

  return SendCommand(&command, sizeof(command))
      .and_then(
          [this, baud_rate](const std::vector<uint8_t>&) -> fpromise::result<void, zx_status_t> {
            fdf::Arena arena('CONF');
            auto result = serial_client_.buffer(arena)->Config(
                baud_rate, fuchsia_hardware_serialimpl::wire::kSerialSetBaudRateOnly);
            if (!result.ok()) {
              return fpromise::error(result.status());
            }
            if (result->is_error()) {
              return fpromise::error(result->error_value());
            }
            return fpromise::ok();
          });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SetBdaddr(
    const std::array<uint8_t, kMacAddrLen>& bdaddr) {
  BcmSetBdaddrCmd command = {
      .header =
          {
              .opcode = kBcmSetBdaddrCmdOpCode,
              .parameter_total_size = sizeof(BcmSetBdaddrCmd) - sizeof(HciCommandHeader),
          },
      .bdaddr =
          {// HCI expects little endian. Swap bytes
           bdaddr[5], bdaddr[4], bdaddr[3], bdaddr[2], bdaddr[1], bdaddr[0]},
  };

  return SendCommand(&command.header, sizeof(command)).and_then([](std::vector<uint8_t>&) {});
}

fpromise::result<std::array<uint8_t, kMacAddrLen>, zx_status_t>
BtHciBroadcom::GetBdaddrFromBootloader() {
  std::array<uint8_t, kMacAddrLen> mac_addr;
  size_t actual_len;

  zx::result compat_device = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (compat_device.is_error()) {
    return fpromise::error(compat_device.status_value());
  }

  fidl::WireResult metadata = fidl::WireCall(compat_device.value())->GetMetadata();
  if (!metadata.ok()) {
    FDF_LOG(WARNING, "GetMetadata failed: %s", metadata.FormatDescription().c_str());
    return fpromise::error(metadata.error().status());
  }

  if (metadata.value().is_error()) {
    FDF_LOG(WARNING, "GetMetadata failed: %s",
            zx_status_get_string(metadata.value().error_value()));
    return fpromise::error(metadata.value().error_value());
  }

  auto meta_vec = metadata.value().value();
  for (auto& entry : meta_vec->metadata) {
    if (entry.type == DEVICE_METADATA_MAC_ADDRESS) {
      entry.data.get_size(&actual_len);
      if (actual_len < kMacAddrLen) {
        return fpromise::error(ZX_ERR_INTERNAL);
      }
      entry.data.read(mac_addr.data(), 0, kMacAddrLen);
      break;
    }
  }

  FDF_LOG(INFO, "got bootloader mac address %02x:%02x:%02x:%02x:%02x:%02x", mac_addr[0],
          mac_addr[1], mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5]);

  return fpromise::ok(mac_addr);
}

fpromise::promise<> BtHciBroadcom::LogControllerFallbackBdaddr() {
  return SendCommand(&kReadBdaddrCmd, sizeof(kReadBdaddrCmd))
      .then([](fpromise::result<std::vector<uint8_t>, zx_status_t>& result) {
        char fallback_addr[18] = "<unknown>";

        if (result.is_ok() && sizeof(ReadBdaddrCommandComplete) == result.value().size()) {
          ReadBdaddrCommandComplete event;
          std::memcpy(&event, result.value().data(), result.value().size());
          // HCI returns data as little endian. Swap bytes
          snprintf(fallback_addr, sizeof(fallback_addr), "%02x:%02x:%02x:%02x:%02x:%02x",
                   event.bdaddr[5], event.bdaddr[4], event.bdaddr[3], event.bdaddr[2],
                   event.bdaddr[1], event.bdaddr[0]);
        }

        FDF_LOG(ERROR, "error getting mac address from bootloader: %s. Fallback address: %s.",
                zx_status_get_string(result.is_ok() ? ZX_OK : result.error()), fallback_addr);
      });
}

constexpr auto kOpenFlags =
    fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kNotDirectory;

fpromise::promise<void, zx_status_t> BtHciBroadcom::LoadFirmware() {
  zx::vmo fw_vmo;
  size_t fw_size;

  // If there's no firmware for this PID, we don't expect the bind to happen without a
  // corresponding entry in the firmware table. Please double-check the PID value and add an entry
  // to the firmware table if it's valid.
  ZX_ASSERT_MSG(kFirmwareMap.find(serial_pid_) != kFirmwareMap.end(), "no mapping for PID: %u",
                serial_pid_);

  std::string full_filename = "/pkg/lib/firmware/";
  full_filename.append(kFirmwareMap.at(serial_pid_).c_str());

  auto client = incoming()->Open<fuchsia_io::File>(full_filename.c_str(), kOpenFlags);
  if (client.is_error()) {
    FDF_LOG(WARNING, "Open firmware file failed: %s", zx_status_get_string(client.error_value()));
    return fpromise::make_error_promise(client.error_value());
  }

  fidl::WireResult backing_memory_result =
      fidl::WireCall(*client)->GetBackingMemory(fuchsia_io::wire::VmoFlags::kRead);
  if (!backing_memory_result.ok()) {
    if (backing_memory_result.is_peer_closed()) {
      FDF_LOG(WARNING, "Failed to get backing memory: Peer closed");
      return fpromise::make_error_promise(ZX_ERR_NOT_FOUND);
    }
    FDF_LOG(WARNING, "Failed to get backing memory: %s",
            zx_status_get_string(backing_memory_result.status()));
    return fpromise::make_error_promise(backing_memory_result.status());
  }

  const auto* backing_memory = backing_memory_result.Unwrap();
  if (backing_memory->is_error()) {
    FDF_LOG(WARNING, "Failed to get backing memory: %s",
            zx_status_get_string(backing_memory->error_value()));
    return fpromise::make_error_promise(backing_memory->error_value());
  }

  zx::vmo& backing_vmo = backing_memory->value()->vmo;
  if (zx_status_t status = backing_vmo.get_prop_content_size(&fw_size); status != ZX_OK) {
    FDF_LOG(WARNING, "Failed to get vmo size: %s", zx_status_get_string(status));
    return fpromise::make_error_promise(status);
  }
  fw_vmo.reset(backing_vmo.release());

  return SendCommand(&kStartFirmwareDownloadCmd, sizeof(kStartFirmwareDownloadCmd))
      .or_else([](zx_status_t& status) -> fpromise::result<std::vector<uint8_t>, zx_status_t> {
        FDF_LOG(ERROR, "could not load firmware file");
        return fpromise::error(status);
      })
      .and_then([this](std::vector<uint8_t>& /*event*/) mutable {
        // give time for placing firmware in download mode
        return executor_->MakeDelayedPromise(zx::duration(kFirmwareDownloadDelay))
            .then([](fpromise::result<>& /*result*/) {
              return fpromise::result<void, zx_status_t>(fpromise::ok());
            });
      })
      .and_then([this, fw_vmo = std::move(fw_vmo), fw_size]() mutable {
        // The firmware is a sequence of HCI commands containing the firmware data as payloads.
        return SendVmoAsCommands(std::move(fw_vmo), fw_size, /*offset=*/0);
      })
      .and_then([this]() -> fpromise::promise<void, zx_status_t> {
        if (is_uart_) {
          // firmware switched us back to 115200. switch back to kTargetBaudRate.
          fdf::Arena arena('CONF');
          auto result = serial_client_.buffer(arena)->Config(
              kDefaultBaudRate, fuchsia_hardware_serialimpl::wire::kSerialSetBaudRateOnly);
          if (!result.ok()) {
            return fpromise::make_result_promise(fpromise::error(result.status()));
          }
          if (result->is_error()) {
            return fpromise::make_result_promise(fpromise::error(result->error_value()));
          }

          return executor_->MakeDelayedPromise(kBaudRateSwitchDelay)
              .then(
                  [this](fpromise::result<>& /*result*/) { return SetBaudRate(kTargetBaudRate); });
        }
        return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
      })
      .and_then([]() { FDF_LOG(INFO, "firmware loaded"); });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::SendVmoAsCommands(zx::vmo vmo, size_t size,
                                                                      size_t offset) {
  if (offset == size) {
    return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
  }

  uint8_t buffer[kMaxHciCommandSize];

  size_t remaining = size - offset;
  size_t read_amount = (remaining > sizeof(buffer) ? sizeof(buffer) : remaining);

  if (read_amount < sizeof(HciCommandHeader)) {
    FDF_LOG(ERROR, "short HCI command in firmware download");
    return fpromise::make_error_promise(ZX_ERR_INTERNAL);
  }

  zx_status_t status = vmo.read(buffer, offset, read_amount);
  if (status != ZX_OK) {
    return fpromise::make_error_promise(status);
  }

  HciCommandHeader header;
  std::memcpy(&header, buffer, sizeof(HciCommandHeader));
  size_t length = header.parameter_total_size + sizeof(header);
  if (read_amount < length) {
    FDF_LOG(ERROR, "short HCI command in firmware download");
    return fpromise::make_error_promise(ZX_ERR_INTERNAL);
  }

  offset += length;

  return SendCommand(buffer, length)
      .then([this, vmo = std::move(vmo), size,
             offset](fpromise::result<std::vector<uint8_t>, zx_status_t>& result) mutable
            -> fpromise::promise<void, zx_status_t> {
        if (result.is_error()) {
          FDF_LOG(ERROR, "SendCommand failed in firmware download: %s",
                  zx_status_get_string(result.error()));
          return fpromise::make_error_promise<zx_status_t>(result.error());
        }

        // Send the next command
        return SendVmoAsCommands(std::move(vmo), size, offset);
      });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::Initialize() {
  zx::channel theirs;
  zx_status_t status = zx::channel::create(/*flags=*/0, &command_channel_, &theirs);
  if (status != ZX_OK) {
    return OnInitializeComplete(status);
  }

  FDF_LOG(DEBUG, "opening command channel");
  auto result = hci_client_.sync()->OpenCommandChannel(std::move(theirs));
  if (!result.ok()) {
    FDF_LOG(ERROR, "OpenCommandChannel failed FIDL error: %s", result.status_string());
    return OnInitializeComplete(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "OpenCommandChannel failed : %s", zx_status_get_string(result->error_value()));
    return OnInitializeComplete(result->error_value());
  }

  FDF_LOG(DEBUG, "sending initial reset command");
  return SendCommand(&kResetCmd, sizeof(kResetCmd))
      .and_then([this](std::vector<uint8_t>&) -> fpromise::promise<void, zx_status_t> {
        if (is_uart_) {
          FDF_LOG(DEBUG, "setting baud rate to %u", kTargetBaudRate);
          // switch baud rate to TARGET_BAUD_RATE
          return SetBaudRate(kTargetBaudRate);
        }
        return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
      })
      .and_then([this]() {
        FDF_LOG(DEBUG, "loading firmware");
        return LoadFirmware();
      })
      .and_then([this]() {
        FDF_LOG(DEBUG, "sending reset command");
        return SendCommand(&kResetCmd, sizeof(kResetCmd));
      })
      .and_then([this](std::vector<uint8_t>&) -> fpromise::promise<void, zx_status_t> {
        FDF_LOG(DEBUG, "setting BDADDR to value from bootloader");
        fpromise::result<std::array<uint8_t, kMacAddrLen>, zx_status_t> bdaddr =
            GetBdaddrFromBootloader();

        if (bdaddr.is_error()) {
          return LogControllerFallbackBdaddr().then(
              [](fpromise::result<>&) -> fpromise::result<void, zx_status_t> {
                return fpromise::ok();
              });
        }

        // send Set BDADDR command
        return SetBdaddr(bdaddr.value());
      })
      .and_then([this]() { return AddNode(); })
      .then([this](fpromise::result<void, zx_status_t>& result) {
        zx_status_t status = result.is_ok() ? ZX_OK : result.error();
        return OnInitializeComplete(status);
      });
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::OnInitializeComplete(zx_status_t status) {
  // We're done with the command channel. Close it so that it can be opened by
  // the host stack after the device becomes visible.
  if (command_channel_.is_valid()) {
    FDF_LOG(DEBUG, "closing command channel");
    command_channel_.reset();
  }

  if (status != ZX_OK) {
    FDF_LOG(ERROR, "device initialization failed: %s", zx_status_get_string(status));
    return fpromise::make_error_promise(status);
  }

  FDF_LOG(INFO, "initialization completed successfully.");
  return fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
}

fpromise::promise<void, zx_status_t> BtHciBroadcom::AddNode() {
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %s", connector.status_string());
    return fpromise::make_error_promise(connector.error_value());
  }

  fidl::Arena args_arena;
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                   .connector(std::move(connector.value()))
                   .class_name("bt-hci")
                   .Build();

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name("bt-hci-broadcom")
                  .devfs_args(devfs)
                  .Build();

  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller end points failed: %s",
            zx_status_get_string(controller_endpoints.error_value()));
    return fpromise::make_error_promise(controller_endpoints.error_value());
  }

  // Create the endpoints of fuchsia_driver_framework::Node protocol for the child node, and hold
  // the client end of it, because no driver will bind to the child node.
  auto child_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (child_node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create child node end points failed: %s",
            zx_status_get_string(child_node_endpoints.error_value()));
    return fpromise::make_error_promise(child_node_endpoints.error_value());
  }

  // Add bt-hci-broadcom child node.
  fpromise::bridge<void, zx_status_t> bridge;
  node_
      ->AddChild(args, std::move(controller_endpoints->server),
                 std::move(child_node_endpoints->server))
      .Then([this, completer = std::move(bridge.completer),
             child_node_client = std::move(child_node_endpoints->client),
             child_controller_client = std::move(controller_endpoints->client)](
                fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>&
                    child_result) mutable {
        if (!child_result.ok()) {
          FDF_LOG(ERROR, "Failed to add bt-hci-broadcom node, FIDL error: %s",
                  child_result.status_string());
          completer.complete_error(child_result.status());
          return;
        }

        if (child_result->is_error()) {
          FDF_LOG(ERROR, "Failed to add bt-hci-broadcom node: %u",
                  static_cast<uint32_t>(child_result->error_value()));
          completer.complete_error(ZX_ERR_INTERNAL);
          return;
        }

        child_node_.Bind(std::move(child_node_client), dispatcher(), this);
        node_controller_.Bind(std::move(child_controller_client), dispatcher(), this);
        completer.complete_ok();
      });

  return bridge.consumer.promise();
}

void BtHciBroadcom::CompleteStart(zx_status_t status) {
  if (start_completer_.has_value()) {
    start_completer_.value()(zx::make_result(status));
    start_completer_.reset();
  } else {
    FDF_LOG(ERROR, "CompleteStart called without start_completer_.");
  }
}

}  // namespace bt_hci_broadcom

FUCHSIA_DRIVER_EXPORT(bt_hci_broadcom::BtHciBroadcom);
