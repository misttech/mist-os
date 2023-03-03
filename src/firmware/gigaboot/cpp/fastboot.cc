// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fastboot.h"

#include <ctype.h>
#include <lib/abr/abr.h>
#include <lib/fastboot/fastboot_base.h>
#include <lib/zircon_boot/zircon_boot.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <xefi.h>

#include <string>

#include <efi/types.h>
#include <fbl/vector.h>
#include <phys/efi/main.h>
#include <pretty/hexdump.h>

#include "boot_zbi_items.h"
#include "gpt.h"
#include "lib/zx/result.h"
#include "utils.h"

namespace gigaboot {

static constexpr std::optional<AbrSlotIndex> ParseAbrSlotStr(std::string_view str, bool allow_r) {
  if (str == "a") {
    return kAbrSlotIndexA;
  } else if (str == "b") {
    return kAbrSlotIndexB;
  } else if (allow_r && str == "r") {
    return kAbrSlotIndexR;
  } else {
    return std::nullopt;
  }
}

zx::result<> Fastboot::ProcessCommand(std::string_view cmd, fastboot::Transport *transport) {
  auto cmd_table = GetCommandCallbackTable();
  for (const CommandCallbackEntry &ele : cmd_table) {
    if (MatchCommand(cmd, ele.name.data())) {
      return (this->*(ele.cmd))(cmd, transport);
    }
  }
  return SendResponse(ResponseType::kFail, "Unsupported command", transport);
}

void Fastboot::DoClearDownload() {}

zx::result<void *> Fastboot::GetDownloadBuffer(size_t total_download_size) {
  if (total_download_size > download_buffer_.size()) {
    return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
  }
  return zx::ok(download_buffer_.data());
}

cpp20::span<Fastboot::VariableEntry> Fastboot::GetVariableTable() {
  static VariableEntry var_entries[] = {
      // Function based variables
      {"max-download-size", {&Fastboot::GetVarMaxDownloadSize}},
      {"current-slot", {&Fastboot::GetVarCurrentSlot}},
      {"slot-last-set-active", {&Fastboot::GetVarSlotLastSetActive}},
      {"slot-retry-count", {&Fastboot::GetVarSlotRetryCount}},
      {"slot-successful", {&Fastboot::GetVarSlotSuccessful}},
      {"slot-unbootable", {&Fastboot::GetVarSlotUnbootable}},
      // Constant based variables
      {"slot-count", {"2"}},
      {"slot-suffixes", {"a,b"}},
  };

  return var_entries;
}

cpp20::span<Fastboot::CommandCallbackEntry> Fastboot::GetCommandCallbackTable() {
  static CommandCallbackEntry cmd_entries[] = {
      {"getvar", &Fastboot::GetVar},
      {"flash", &Fastboot::Flash},
      {"continue", &Fastboot::Continue},
      {"reboot", &Fastboot::Reboot},
      {"reboot-bootloader", &Fastboot::RebootBootloader},
      {"reboot-recovery", &Fastboot::RebootRecovery},
      {"set_active", &Fastboot::SetActive},
      {"oem gpt-init", &Fastboot::GptInit},
      {"oem add-staged-bootloader-file", &Fastboot::OemAddStagedBootloaderFile},
      {"oem efi-getvarinfo", &Fastboot::EfiGetVarInfo},
      {"oem efi-getvarnames", &Fastboot::EfiGetVarNames},
      {"oem efi-getvar", &Fastboot::EfiGetVar},
      {"oem efi-dumpvars", &Fastboot::EfiDumpVars},
  };

  return cmd_entries;
}

zx::result<> Fastboot::Reboot(std::string_view cmd, fastboot::Transport *transport) {
  return DoReboot(RebootMode::kNormal, cmd, transport);
}

zx::result<> Fastboot::RebootBootloader(std::string_view cmd, fastboot::Transport *transport) {
  return DoReboot(RebootMode::kBootloader, cmd, transport);
}

zx::result<> Fastboot::RebootRecovery(std::string_view cmd, fastboot::Transport *transport) {
  return DoReboot(RebootMode::kRecovery, cmd, transport);
}

zx::result<> Fastboot::DoReboot(RebootMode reboot_mode, std::string_view cmd,
                                fastboot::Transport *transport) {
  if (!SetRebootMode(reboot_mode)) {
    return SendResponse(ResponseType::kFail, "Failed to set reboot mode", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  // ResetSystem() below won't return. Thus sends a OKAY response first in case we succeed.
  zx::result<> res = SendResponse(ResponseType::kOkay, "", transport);
  if (res.is_error()) {
    return res;
  }

  efi_status status =
      gEfiSystemTable->RuntimeServices->ResetSystem(EfiResetCold, EFI_SUCCESS, 0, NULL);
  if (status != EFI_SUCCESS) {
    printf("Failed to reboot: %s\n", EfiStatusToString(status));
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok();
}

zx::result<> Fastboot::SetActive(std::string_view cmd, fastboot::Transport *transport) {
  CommandArgs args;
  ExtractCommandArgs(cmd, ":", args);

  if (args.num_args < 2) {
    return SendResponse(ResponseType::kFail, "missing slot name", transport);
  }

  std::optional<AbrSlotIndex> idx = ParseAbrSlotStr(args.args[1], false);
  if (!idx) {
    return SendResponse(ResponseType::kFail, "slot name is invalid", transport);
  }

  AbrOps abr_ops = GetAbrOps();
  AbrResult res = AbrMarkSlotActive(&abr_ops, *idx);
  if (res != kAbrResultOk) {
    return SendResponse(ResponseType::kFail, "Failed to set slot", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

// Used to allow multiple single-type lambdas in std::visit
// instead of a single lambda with multiple constexpr if branches
template <class... Ts>
struct overload : Ts... {
  using Ts::operator()...;
};
template <class... Ts>
overload(Ts...) -> overload<Ts...>;

zx::result<> Fastboot::GetVar(std::string_view cmd, fastboot::Transport *transport) {
  CommandArgs args;
  ExtractCommandArgs(cmd, ":", args);
  if (args.num_args < 2) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }
  auto var_table = GetVariableTable();
  for (const VariableEntry &ele : var_table) {
    if (args.args[1] == ele.name) {
      return std::visit(
          overload{
              [this, &args, transport](VarFunc arg) { return (this->*(arg))(args, transport); },
              [transport](std::string_view arg) {
                return SendResponse(ResponseType::kOkay, arg, transport);
              },
          },
          ele.var);
    }
  }

  return SendResponse(ResponseType::kFail, "Unknown variable", transport);
}

zx::result<> Fastboot::GetVarMaxDownloadSize(const CommandArgs &, fastboot::Transport *transport) {
  char size_str[16] = {0};
  snprintf(size_str, sizeof(size_str), "0x%08zx", download_buffer_.size());
  return SendResponse(ResponseType::kOkay, size_str, transport);
}

zx::result<> Fastboot::GetVarCurrentSlot(const CommandArgs &, fastboot::Transport *transport) {
  AbrOps abr_ops = GetAbrOps();

  char const *slot_str;
  AbrSlotIndex slot = AbrGetBootSlot(&abr_ops, false, nullptr);
  switch (slot) {
    case kAbrSlotIndexA:
      slot_str = "a";
      break;
    case kAbrSlotIndexB:
      slot_str = "b";
      break;
    case kAbrSlotIndexR:
      slot_str = "r";
      break;
    default:
      slot_str = "";
      break;
  }

  return SendResponse(ResponseType::kOkay, slot_str, transport);
}

zx::result<> Fastboot::GetVarSlotLastSetActive(const CommandArgs &,
                                               fastboot::Transport *transport) {
  AbrOps abr_ops = GetAbrOps();
  AbrSlotIndex slot;
  AbrResult res = AbrGetSlotLastMarkedActive(&abr_ops, &slot);
  if (res != kAbrResultOk) {
    return SendResponse(ResponseType::kFail, "Failed to get slot last set active", transport);
  }
  // The slot is guaranteed not to be r if the result is okay.
  const char *slot_str = slot == kAbrSlotIndexA ? "a" : "b";

  return SendResponse(ResponseType::kOkay, slot_str, transport);
}

zx::result<> Fastboot::GetVarSlotRetryCount(const CommandArgs &args,
                                            fastboot::Transport *transport) {
  if (args.num_args < 3) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }

  std::optional<AbrSlotIndex> idx = ParseAbrSlotStr(args.args[2], false);
  if (!idx) {
    return SendResponse(ResponseType::kFail, "slot name is invalid", transport);
  }

  AbrOps abr_ops = GetAbrOps();
  AbrSlotInfo info;
  AbrResult res = AbrGetSlotInfo(&abr_ops, *idx, &info);
  if (res != kAbrResultOk) {
    return SendResponse(ResponseType::kFail, "Failed to get slot retry count", transport);
  }

  char retry_str[16] = {0};
  snprintf(retry_str, sizeof(retry_str), "%u", info.num_tries_remaining);

  return SendResponse(ResponseType::kOkay, retry_str, transport);
}

zx::result<> Fastboot::GetVarSlotSuccessful(const CommandArgs &args,
                                            fastboot::Transport *transport) {
  if (args.num_args < 3) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }

  std::optional<AbrSlotIndex> idx = ParseAbrSlotStr(args.args[2], true);
  if (!idx) {
    return SendResponse(ResponseType::kFail, "slot name is invalid", transport);
  }

  AbrOps abr_ops = GetAbrOps();
  AbrSlotInfo info;
  AbrResult res = AbrGetSlotInfo(&abr_ops, *idx, &info);
  if (res != kAbrResultOk) {
    return SendResponse(ResponseType::kFail, "Failed to get slot successful", transport);
  }

  return SendResponse(ResponseType::kOkay, info.is_marked_successful ? "yes" : "no", transport);
}

zx::result<> Fastboot::GetVarSlotUnbootable(const CommandArgs &args,
                                            fastboot::Transport *transport) {
  if (args.num_args < 3) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }

  std::optional<AbrSlotIndex> idx = ParseAbrSlotStr(args.args[2], true);
  if (!idx) {
    return SendResponse(ResponseType::kFail, "slot name is invalid", transport);
  }

  AbrOps abr_ops = GetAbrOps();
  AbrSlotInfo info;
  AbrResult res = AbrGetSlotInfo(&abr_ops, *idx, &info);
  if (res != kAbrResultOk) {
    return SendResponse(ResponseType::kFail, "Failed to get slot unbootable", transport);
  }

  return SendResponse(ResponseType::kOkay, info.is_bootable ? "no" : "yes", transport);
}

zx::result<> Fastboot::Flash(std::string_view cmd, fastboot::Transport *transport) {
  CommandArgs args;
  ExtractCommandArgs(cmd, ":", args);
  if (args.num_args < 2) {
    return SendResponse(ResponseType::kFail, "Not enough argument", transport);
  }

  ZX_ASSERT(args.args[1].size() < fastboot::kMaxCommandPacketSize);
  char part_name[fastboot::kMaxCommandPacketSize] = {0};
  memcpy(part_name, args.args[1].data(), args.args[1].size());
  size_t write_size;
  bool res = zb_ops_.write_to_partition(&zb_ops_, part_name, 0, total_download_size(),
                                        download_buffer_.data(), &write_size);

  if (!res || write_size != total_download_size()) {
    return SendResponse(ResponseType::kFail, "Failed to write to partition", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::GptInit(std::string_view cmd, fastboot::Transport *transport) {
  auto device = FindEfiGptDevice();
  if (!device.is_ok()) {
    return SendResponse(ResponseType::kFail, "Failed to get block device", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  auto res = device.value().Reinitialize();
  if (!res.is_ok()) {
    return SendResponse(ResponseType::kFail, "Failed to reinitialize partiions", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::EfiGetVarInfo(std::string_view cmd, fastboot::Transport *transport) {
  auto info = efi_variables_->EfiQueryVariableInfo();
  if (info.is_error()) {
    return SendResponse(ResponseType::kFail, "QueryVariableInfo() failed", transport);
  }

  printer_(
      "\n"
      "  Max Storage Size: %" PRIu64
      "\n"
      "  Remaining Variable Storage Size: %" PRIu64
      "\n"
      "  Max Variable Size: %" PRIu64 "\n",
      info.value().max_var_storage_size, info.value().remaining_var_storage_size,
      info.value().max_var_size);

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::EfiGetVarNames(std::string_view cmd, fastboot::Transport *transport) {
  for (auto v_id : *efi_variables_) {
    if (!v_id.IsValid()) {
      return SendResponse(ResponseType::kFail, "EfiVariableName iteration failed", transport);
    }

    auto v_id_utf8 = efi_variables_->Ucs2ToStr(v_id.name);
    if (v_id_utf8.is_error()) {
      printer_(xefi_strerror(v_id_utf8.error_value()));
      continue;
    }
    const auto &guid = v_id.vendor_guid;
    printer_("%s %s\n", ToStr(guid).data(), v_id_utf8.value().data());
  }
  printer_("\n");

  return SendResponse(ResponseType::kOkay, "", transport);
}

void hexdump_printer_printf(void *printf_arg, const char *fmt, ...) {
  Fastboot *obj = static_cast<Fastboot *>(printf_arg);
  va_list args;
  va_start(args, fmt);
  obj->vprinter_(fmt, args);
  va_end(args);
}

// This function expects `cmd` in following format:
// oem efi-getvar <var_name> [<guid>]
//
// E.g.
//  oem efi-getvar BootOrder
//  oem efi-getvar BootOrder 8be4df61-93ca-11d2-aa0d-00e098032b8c
zx::result<> Fastboot::EfiGetVar(std::string_view cmd, fastboot::Transport *transport) {
  CommandArgs args;
  ExtractCommandArgs(cmd, " ", args);

  if (args.num_args < 3 || args.num_args > 4) {
    return SendResponse(ResponseType::kFail,
                        "Bad number of arguments."
                        " Expected format: oem efi-getvar <var_name> [<guid>]",
                        transport);
  }

  const std::string_view in_var_name = args.args[2];
  auto res_var_name = efi_variables_->StrToUcs2(in_var_name);
  if (res_var_name.is_error()) {
    printer_("UTF8->UC2 convertion failed for variable name: '%s'\n", in_var_name.data());
    return SendResponse(ResponseType::kFail, "UTF8->UC2 convertion failed for variable name",
                        transport);
  }
  fbl::Vector<char16_t> &var_name = res_var_name.value();

  efi_guid guid;
  if (args.num_args == 4) {
    std::string_view guid_str = args.args[3];
    auto res_guid = ToGuid(guid_str);
    if (res_guid.is_error()) {
      printer_("Vendor GUID parsing failed (%s) for: '%s'\n", xefi_strerror(res_guid.error_value()),
               guid_str.data());
      return SendResponse(ResponseType::kFail, "Vendor GUID parsing failed", transport);
    }
    guid = res_guid.value();
  } else if (args.num_args == 3) {
    auto res_guid = efi_variables_->GetGuid(var_name);
    if (res_guid.is_error()) {
      printer_("Vendor GUID search failed (%s) for: '%s'\n", xefi_strerror(res_guid.error_value()),
               in_var_name.data());
      if (res_guid.error_value() == EFI_INVALID_PARAMETER) {
        return SendResponse(ResponseType::kFail,
                            "Multiple entries found with specified name. Please provide GUID",
                            transport);
      } else {
        return SendResponse(ResponseType::kFail, "Vendor GUID search failed", transport);
      }
    }
    guid = res_guid.value();
  }

  // Print VariableName
  EfiVariables::EfiVariableId v_id(var_name, guid);
  auto v_id_utf8 = efi_variables_->Ucs2ToStr(v_id.name);
  if (v_id_utf8.is_error()) {
    const auto err_str = "Failed to convert UCS2 variable name to UTF8\n";
    printer_(err_str);
    return SendResponse(ResponseType::kFail, err_str, transport);
  }
  printer_("%s %s:\n", ToStr(guid).data(), v_id_utf8.value().data());

  // Get and print Value
  auto res_get_var = efi_variables_->EfiGetVariable(v_id);
  if (res_get_var.is_error()) {
    return SendResponse(ResponseType::kFail, "GetVariable() failed", transport);
  }
  hexdump8_very_ex(res_get_var.value().data(), res_get_var.value().size(), 0,
                   hexdump_printer_printf, this);

  return SendResponse(ResponseType::kOkay, "", transport);
}

int Fastboot::printer_(const char *fmt, ...) {
  va_list args;
  va_start(args, fmt);
  int res = vprinter_(fmt, args);
  va_end(args);
  return res;
}

zx::result<> Fastboot::EfiDumpVars(std::string_view cmd, fastboot::Transport *transport) {
  for (const auto &v_id : *efi_variables_) {
    if (!v_id.IsValid())
      return SendResponse(ResponseType::kFail, "EfiVariableName iteration failed", transport);

    auto v_id_utf8 = efi_variables_->Ucs2ToStr(v_id.name);
    if (v_id_utf8.is_error()) {
      printer_("Failed to convert UCS2 variable name to UTF8\n");
      continue;
    }
    const auto &guid = v_id.vendor_guid;
    printer_("%s %s:\n", ToStr(guid).data(), v_id_utf8.value().data());

    auto res = efi_variables_->EfiGetVariable(v_id);
    if (res.is_error()) {
      printer_("Failed to get variable\n");
      continue;
    }

    auto &val = res.value();
    hexdump8_very_ex(val.data(), val.size(), 0, hexdump_printer_printf, this);
  }
  printer_("\n");

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::Continue(std::string_view cmd, fastboot::Transport *transport) {
  continue_ = true;
  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::OemAddStagedBootloaderFile(std::string_view cmd,
                                                  fastboot::Transport *transport) {
  CommandArgs args;
  ExtractCommandArgs(cmd, " ", args);
  if (args.num_args != 3) {
    return SendResponse(ResponseType::kFail, "Not enough argument", transport);
  }

  zbi_result_t res =
      AddBootloaderFiles(args.args[2].data(), download_buffer_.data(), total_download_size());
  if (res != ZBI_RESULT_OK) {
    printf("Failed to append zbi_files: %d\n", res);
    return SendResponse(ResponseType::kFail, "Failed to initialize zbi file", transport,
                        zx::error(ZX_ERR_INTERNAL));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

// The transport implementation for a TCP fastboot packet.
class PacketTransport : public fastboot::Transport {
 public:
  PacketTransport(TcpTransportInterface &interface, size_t packet_size)
      : interface_(&interface), packet_size_(packet_size) {}

  zx::result<size_t> ReceivePacket(void *dst, size_t capacity) override {
    if (packet_size_ > capacity) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }

    if (!interface_->Read(dst, packet_size_)) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    return zx::ok(packet_size_);
  }

  // Peek the size of the next packet.
  size_t PeekPacketSize() override { return packet_size_; }

  zx::result<> Send(std::string_view packet) override {
    // Prepend a length prefix.
    size_t size = packet.size();
    uint64_t be_size = ToBigEndian(size);
    if (!interface_->Write(&be_size, sizeof(be_size))) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (!interface_->Write(packet.data(), size)) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    return zx::ok();
  }

 private:
  TcpTransportInterface *interface_ = nullptr;
  size_t packet_size_ = 0;
};

void FastbootTcpSession(TcpTransportInterface &interface, Fastboot &fastboot) {
  // Whatever we receive, sends a handshake message first to improve performance.
  printf("Fastboot connection established\n");
  if (!interface.Write("FB01", 4)) {
    printf("Failed to write handshake message\n");
    return;
  }

  char handshake_buffer[kFastbootHandshakeMessageLength + 1] = {0};
  if (!interface.Read(handshake_buffer, kFastbootHandshakeMessageLength)) {
    printf("Failed to read handshake message\n");
    return;
  }

  // We expect "FBxx", where xx is a numeric value
  if (strncmp(handshake_buffer, "FB", 2) != 0 || !isdigit(handshake_buffer[2]) ||
      !isdigit(handshake_buffer[3])) {
    printf("Invalid handshake message %s\n", handshake_buffer);
    return;
  }

  while (true) {
    // Each fastboot packet is a length-prefixed data sequence. Read the length
    // prefix first.
    uint64_t packet_length = 0;
    if (!interface.Read(&packet_length, sizeof(packet_length))) {
      printf("Failed to read length prefix. Remote client might be disconnected\n");
      return;
    }

    // Process the length prefix. Convert big-endian to integer.
    packet_length = BigToHostEndian(packet_length);

    // Construct and pass a packet transport to fastboot.
    PacketTransport packet(interface, packet_length);
    zx::result<> ret = fastboot.ProcessPacket(&packet);
    if (ret.is_error()) {
      printf("Failed to process fastboot packet\n");
      return;
    }

    if (fastboot.IsContinue()) {
      printf("Resuming boot...\n");
      return;
    }
  }
}

}  // namespace gigaboot
