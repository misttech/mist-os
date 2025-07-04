// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.buildinfo/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fastboot/fastboot.h>
#include <lib/fdio/directory.h>
#include <zircon/status.h>

#include <optional>
#include <string_view>
#include <thread>
#include <vector>

#include <fbl/unique_fd.h>
#include <sdk/lib/syslog/cpp/macros.h>

#include "lib/zx/result.h"
#include "payload-streamer.h"
#include "sparse_format.h"
#include "src/firmware/lib/fastboot/rust/ffi_c/bindings.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace fastboot {
namespace {

constexpr char kFastbootLogTag[] = __FILE__;

struct FlashPartitionInfo {
  std::string_view partition;
  std::optional<fuchsia_paver::wire::Configuration> configuration;
};

FlashPartitionInfo GetPartitionInfo(std::string_view partition_label) {
  size_t len = partition_label.length();
  if (len < 2) {
    return {partition_label, std::nullopt};
  }

  FlashPartitionInfo ret;
  ret.partition = partition_label.substr(0, len - 2);
  std::string_view slot_suffix = partition_label.substr(len - 2, 2);
  // TODO(b/241150035): Some platforms such as x64 still use legacy kernel partition name
  // zircon-a/b/r. Hardcode these cases for backward compatibility. Once all products migrate to new
  // name. Remove them.
  if (slot_suffix == "_a" || partition_label == "zircon-a") {
    ret.configuration = fuchsia_paver::wire::Configuration::kA;
  } else if (slot_suffix == "_b" || partition_label == "zircon-b") {
    ret.configuration = fuchsia_paver::wire::Configuration::kB;
  } else if (slot_suffix == "_r" || partition_label == "zircon-r") {
    ret.configuration = fuchsia_paver::wire::Configuration::kRecovery;
  } else {
    ret.partition = partition_label;
  }

  return ret;
}

bool IsAndroidSparseImage(const void* img, size_t size) {
  if (size < sizeof(sparse_header_t)) {
    return false;
  }
  sparse_header_t header;
  memcpy(&header, img, sizeof(sparse_header_t));
  return header.magic == SPARSE_HEADER_MAGIC;
}

}  // namespace

const std::vector<Fastboot::CommandEntry>& Fastboot::GetCommandTable() {
  // Using a static pointer and allocate with `new` so that the static instance
  // never gets deleted.
  static const std::vector<CommandEntry>* kCommandTable = new std::vector<CommandEntry>({
      {
          .name = "getvar",
          .cmd = &Fastboot::GetVar,
      },
      {
          .name = "flash",
          .cmd = &Fastboot::Flash,
      },
      {
          .name = "set_active",
          .cmd = &Fastboot::SetActive,
      },
      {
          .name = "reboot",
          .cmd = &Fastboot::Reboot,
      },
      {
          .name = "continue",
          .cmd = &Fastboot::Continue,
      },
      {
          .name = "reboot-bootloader",
          .cmd = &Fastboot::RebootBootloader,
      },
      {
          .name = "oem add-staged-bootloader-file",
          .cmd = &Fastboot::OemAddStagedBootloaderFile,
      },
      {
          .name = "oem init-partition-tables",
          .cmd = &Fastboot::OemInitPartitionTables,
      },
      {
          .name = "oem install-from-usb",
          .cmd = &Fastboot::OemInstallFromUsb,
      },
      {
          .name = "oem wipe-partition-tables",
          .cmd = &Fastboot::OemWipePartitionTables,
      },
  });
  return *kCommandTable;
}

const Fastboot::VariableHashTable& Fastboot::GetVariableTable() {
  // Using a static pointer and allocate with `new` so that the static instance
  // never gets deleted.
  static const VariableHashTable* kVariableTable = new VariableHashTable({
      {"max-download-size", &Fastboot::GetVarMaxDownloadSize},
      {"slot-count", &Fastboot::GetVarSlotCount},
      {"is-userspace", &Fastboot::GetVarIsUserspace},
      {"hw-revision", &Fastboot::GetVarHwRevision},
      {"product", &Fastboot::GetVarHwRevision},
      {"version", &Fastboot::GetVarVersion},
      {"all", &Fastboot::GetVarAll},
  });
  return *kVariableTable;
}

Fastboot::Fastboot(size_t max_download_size) : max_download_size_(max_download_size) {}

Fastboot::Fastboot(size_t max_download_size, fidl::ClientEnd<fuchsia_io::Directory> svc_root)
    : max_download_size_(max_download_size), svc_root_(std::move(svc_root)) {}

zx::result<> Fastboot::ProcessCommand(std::string_view command, Transport* transport) {
  for (const CommandEntry& cmd : GetCommandTable()) {
    if (MatchCommand(command, cmd.name)) {
      return (this->*cmd.cmd)(command.data(), transport);
    }
  }

  return SendResponse(ResponseType::kFail, "Unsupported command", transport);
}

void Fastboot::DoClearDownload() { download_vmo_mapper_.Reset(); }

zx::result<void*> Fastboot::GetDownloadBuffer(size_t total_download_size) {
  if (zx_status_t ret = download_vmo_mapper_.CreateAndMap(total_download_size, "fastboot download");
      ret != ZX_OK) {
    return zx::error(ret);
  }

  if (zx_status_t ret = download_vmo_mapper_.vmo().set_prop_content_size(total_download_size);
      ret != ZX_OK) {
    return zx::error(ret);
  }

  return zx::ok(download_vmo_mapper_.start());
}

zx::result<> Fastboot::GetVar(const std::string& command, Transport* transport) {
  std::vector<std::string_view> args =
      fxl::SplitString(command, ":", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (args.size() < 2) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }

  const VariableHashTable& var_table = GetVariableTable();
  const VariableHashTable::const_iterator find_res = var_table.find(args[1].data());
  if (find_res == var_table.end()) {
    return SendResponse(ResponseType::kFail, "Unknown variable", transport);
  }

  zx::result<std::string> var_ret = (this->*(find_res->second))(args, transport);
  if (var_ret.is_error()) {
    return SendResponse(ResponseType::kFail, "Fail to get variable", transport,
                        zx::error(var_ret.status_value()));
  }

  return SendResponse(ResponseType::kOkay, var_ret.value(), transport);
}

zx::result<std::string> Fastboot::GetVarVersion(const std::vector<std::string_view>&, Transport*) {
  return zx::ok("0.4");
}

zx::result<std::string> Fastboot::GetVarAll(const std::vector<std::string_view>& args,
                                            Transport* transport) {
  zx::result<std::string> res(zx::ok("done"));

  const VariableHashTable& var_table = GetVariableTable();
  for (const auto& [name, func] : var_table) {
    if (name == "all") {
      continue;
    }

    std::vector<std::string_view> var_args = {"getvar", name};
    zx::result<std::string> var_ret = (this->*(func))(var_args, transport);

    if (var_ret.is_error()) {
      res = zx::ok("not all variables were retrieved successfully");
    }

    std::string response = fxl::StringPrintf(
        "%s: %s", name.c_str(),
        var_ret.value_or(std::string("[error: ") + var_ret.status_string() + "]").c_str());

    zx::result<> ret = SendResponse(ResponseType::kInfo, response, transport);
    if (ret.is_error()) {
      return zx::error(ret.status_value());
    }
  }

  return res;
}

zx::result<std::string> Fastboot::GetVarMaxDownloadSize(const std::vector<std::string_view>&,
                                                        Transport*) {
  return zx::ok(fxl::StringPrintf("0x%08zx", max_download_size_));
}

zx::result<std::string> Fastboot::GetVarHwRevision(const std::vector<std::string_view>&,
                                                   Transport*) {
  auto svc_root = GetSvcRoot();
  if (svc_root.is_error()) {
    return zx::error(svc_root.status_value());
  }

  auto connect_result = component::ConnectAt<fuchsia_buildinfo::Provider>(svc_root.value());
  if (connect_result.is_error()) {
    return zx::error(connect_result.status_value());
  }

  auto provider = fidl::WireSyncClient(std::move(connect_result.value()));

  auto resp = provider->GetBuildInfo();
  if (!resp.ok()) {
    return zx::error(resp.status());
  }

  return zx::ok(resp->build_info.board_config().data());
}

zx::result<std::string> Fastboot::GetVarSlotCount(const std::vector<std::string_view>&,
                                                  Transport* transport) {
  auto boot_manager_res = FindBootManager();
  if (boot_manager_res.is_error()) {
    auto ret = SendResponse(ResponseType::kFail, "Failed to find boot manager", transport,
                            zx::error(boot_manager_res.status_value()));
    return zx::error(ret.status_value());
  }
  // `fastboot set_active` only cares whether the device has >1 slots. Doesn't care how many
  // exactly.
  return boot_manager_res.value()->QueryCurrentConfiguration().ok() ? zx::ok("2") : zx::ok("1");
}

zx::result<std::string> Fastboot::GetVarIsUserspace(const std::vector<std::string_view>&,
                                                    Transport*) {
  return zx::ok("yes");
}

zx::result<fidl::UnownedClientEnd<fuchsia_io::Directory>> Fastboot::GetSvcRoot() {
  // If `svc_root_` is not set, use the system svc root.
  if (!svc_root_) {
    auto svc_root = component::OpenServiceRoot();
    if (svc_root.is_error()) {
      FX_LOGST(ERROR, kFastbootLogTag)
          << "Failed to connect to svc root " << svc_root.status_string();
      return zx::error(ZX_ERR_INTERNAL);
    }
    svc_root_ = std::move(*svc_root);
  }

  return zx::ok(fidl::UnownedClientEnd<fuchsia_io::Directory>(svc_root_));
}

zx::result<fidl::WireSyncClient<fuchsia_paver::Paver>> Fastboot::ConnectToPaver() {
  // Connect to the paver
  auto svc_root = GetSvcRoot();
  if (svc_root.is_error()) {
    return zx::error(svc_root.status_value());
  }

  auto paver_svc = component::ConnectAt<fuchsia_paver::Paver>(*svc_root);
  if (!paver_svc.is_ok()) {
    FX_LOGST(ERROR, kFastbootLogTag) << "Unable to open /svc/fuchsia.paver.Paver";
    return zx::error(paver_svc.error_value());
  }

  return zx::ok(fidl::WireSyncClient(std::move(*paver_svc)));
}

fuchsia_mem::wire::Buffer Fastboot::GetWireBufferFromDownload() {
  fuchsia_mem::wire::Buffer buf;
  buf.size = download_vmo_mapper_.size();
  buf.vmo = download_vmo_mapper_.Release();
  return buf;
}

zx::result<> Fastboot::WriteFirmware(fuchsia_paver::wire::Configuration config,
                                     std::string_view firmware_type, Transport* transport,
                                     fidl::WireSyncClient<fuchsia_paver::DataSink>& data_sink) {
  auto ret = data_sink->WriteFirmware(config, fidl::StringView::FromExternal(firmware_type),
                                      GetWireBufferFromDownload());
  if (ret.status() != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to invoke paver bootloader write", transport,
                        zx::error(ret.status()));
  }

  if (ret.value().result.is_status() && ret.value().result.status() != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to write bootloader", transport,
                        zx::error(ret.value().result.status()));
  }

  if (ret.value().result.is_unsupported() && ret.value().result.unsupported()) {
    return SendResponse(ResponseType::kFail, "Firmware type is not supported", transport);
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::WriteAsset(fuchsia_paver::wire::Configuration config,
                                  fuchsia_paver::wire::Asset asset, Transport* transport,
                                  fidl::WireSyncClient<fuchsia_paver::DataSink>& data_sink) {
  auto ret = data_sink->WriteAsset(config, asset, GetWireBufferFromDownload());
  zx_status_t status = ret.status() == ZX_OK ? ret.value().status : ret.status();
  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to flash asset", transport, zx::error(status));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::Flash(const std::string& command, Transport* transport) {
  std::vector<std::string_view> args =
      fxl::SplitString(command, ":", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (args.size() < 2) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }
  FlashPartitionInfo info = GetPartitionInfo(args[1]);
  const bool is_sparse =
      IsAndroidSparseImage(download_vmo_mapper_.start(), download_vmo_mapper_.size());

  if (info.partition == "super") {
    if (!is_sparse) {
      return SendResponse(ResponseType::kFail, "super must be in Android sparse format.",
                          transport);
    }
  } else if (is_sparse) {
    return SendResponse(ResponseType::kFail, "Android sparse image is not supported.", transport);
  }

  auto paver_client_res = ConnectToPaver();
  if (paver_client_res.is_error()) {
    return SendResponse(ResponseType::kFail, "Failed to connect to paver", transport,
                        zx::error(paver_client_res.status_value()));
  }

  // Connect to the data sink
  auto data_sink_endpoints = fidl::CreateEndpoints<fuchsia_paver::DataSink>();
  if (data_sink_endpoints.is_error()) {
    return SendResponse(ResponseType::kFail, "Unable to create data sink endpoint", transport,
                        zx::error(data_sink_endpoints.status_value()));
  }
  auto [data_sink_local, data_sink_remote] = std::move(*data_sink_endpoints);
  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  (void)paver_client_res.value()->FindDataSink(std::move(data_sink_remote));
  fidl::WireSyncClient data_sink{std::move(data_sink_local)};

  if (info.partition == "bootloader") {
    // If abr suffix is not given, assume that firmware ABR is not supported and just provide a
    // A slot configuration. It will be ignored by the paver.
    fuchsia_paver::wire::Configuration config =
        info.configuration ? *info.configuration : fuchsia_paver::wire::Configuration::kA;
    std::string_view firmware_type = args.size() == 3 ? args[2] : "";
    return WriteFirmware(config, firmware_type, transport, data_sink);
  } else if (info.partition == "fuchsia-esp") {
    // x64 platform uses 'fuchsia-esp' for bootloader partition . We should eventually move to use
    // "bootloader"
    // For legacy `fuchsia-esp` we don't consider firmware ABR or type.
    return WriteFirmware(fuchsia_paver::wire::Configuration::kA, "", transport, data_sink);
  } else if (info.partition == "zircon" && info.configuration) {
    return WriteAsset(*info.configuration, fuchsia_paver::wire::Asset::kKernel, transport,
                      data_sink);
  } else if (info.partition == "vbmeta" && info.configuration) {
    return WriteAsset(*info.configuration, fuchsia_paver::wire::Asset::kVerifiedBootMetadata,
                      transport, data_sink);
  } else if (info.partition == "fvm") {
    auto ret = data_sink->WriteOpaqueVolume(GetWireBufferFromDownload());

    zx_status_t status = ZX_OK;
    if (ret.status() != ZX_OK) {
      status = ret.status();
    } else if (ret->is_error()) {
      status = ret->error_value();
    }

    if (status != ZX_OK) {
      return SendResponse(ResponseType::kFail, "Failed to flash opaque fvm", transport,
                          zx::error(status));
    }

    return SendResponse(ResponseType::kOkay, "", transport);
  } else if (info.partition == "fvm.sparse") {
    // Flashing the sparse format FVM image via the paver. Note that at the time this code is
    // written, the format of FVM for fuchsia has not reached at a stable point yet. However, the
    // implementation of the paver fidl interface `WriteVolumes()` depends on the format of the FVM.
    // Therefore, it is important make sure that the device is running the latest version of paver
    // before using this fastboot command. This typically means flashing the latest kernel and
    // reboot first. Otherwise, if FVM format changes and the currently running paver is not
    // up-to-date, the FVM may be flashed wrongly.
    auto streamer_endpoints = fidl::CreateEndpoints<fuchsia_paver::PayloadStream>();
    if (streamer_endpoints.is_error()) {
      return SendResponse(ResponseType::kFail, "Failed to create payload streamer", transport,
                          zx::error(streamer_endpoints.status_value()));
    }
    auto [client, server] = std::move(*streamer_endpoints);

    // Launch thread which implements interface.
    async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
    internal::PayloadStreamer streamer(std::move(server), download_vmo_mapper_.start(),
                                       download_vmo_mapper_.size());
    loop.StartThread("fastboot-payload-stream");

    auto result = data_sink->WriteVolumes(std::move(client));
    zx_status_t status = result.ok() ? result.value().status : result.status();
    if (status != ZX_OK) {
      return SendResponse(ResponseType::kFail, "Failed to write fvm", transport, zx::error(status));
    }

    download_vmo_mapper_.Reset();
    return SendResponse(ResponseType::kOkay, "", transport);
  } else if (info.partition == "gpt-meta") {
    // gpt-meta is a pseudo-partition; we don't write the contents directly to the GPT but instead
    // it provides some higher-level information about what the GPT should look like and it's up to
    // the device implementation to translate that to an actual GPT.

    if (info.configuration) {
      return SendResponse(ResponseType::kFail, "gpt-meta doesn't support slots", transport,
                          zx::error(ZX_ERR_INVALID_ARGS));
    }

    // For now we only support a single input format of a file containing the word "default", which
    // means to write the default GPT exactly as `oem init-partition-tables` would. The reason we
    // provide this alias is to simplify `ffx flash` by tying into the existing partition flash
    // mechanism rather than having to teach it about the `oem init-partition-tables` command.
    std::string_view contents(reinterpret_cast<const char*>(download_vmo_mapper_.start()),
                              download_vmo_mapper_.size());
    if (contents == kGptMetaDefault) {
      return OemInitPartitionTables(command, transport);
    } else {
      return SendResponse(ResponseType::kFail, "Invalid gpt-meta contents", transport,
                          zx::error(ZX_ERR_INVALID_ARGS));
    }
  } else if (info.partition == "super") {
    // TODO(https://fxbug.dev/397515768): We don't yet handle the presence or absence of the -w
    // flag which is used to indicate if we should wipe userdata or not. For now, this will always
    // overwrite userdata regardless of this flag.
    auto ret = data_sink->WriteSparseVolume(GetWireBufferFromDownload());
    if (ret.status() != ZX_OK) {
      return SendResponse(ResponseType::kFail,
                          "Failed to invoke fuchsia.paver/DataSink.WriteSparseVolume", transport,
                          zx::error(ret.status()));
    }
    if (ret.value().is_error()) {
      return SendResponse(ResponseType::kFail, "Failed to flash super", transport,
                          zx::error(ret.value().error_value()));
    }
    return SendResponse(ResponseType::kOkay, "", transport);

  } else {
    return SendResponse(ResponseType::kFail, "Unsupported partition", transport);
  }

  return zx::ok();
}

zx::result<fidl::WireSyncClient<fuchsia_paver::BootManager>> Fastboot::FindBootManager() {
  auto paver_client_res = ConnectToPaver();
  if (!paver_client_res.is_ok()) {
    return zx::error(paver_client_res.status_value());
  }

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_paver::BootManager>();
  if (endpoints.is_error()) {
    FX_LOGST(ERROR, kFastbootLogTag) << "Failed to create endpoint";
    return zx::error(endpoints.status_value());
  }

  fidl::Status res = paver_client_res.value()->FindBootManager(std::move(endpoints->server));
  if (!res.ok()) {
    FX_LOGST(ERROR, kFastbootLogTag) << "Failed to find boot manager";
    return zx::error(res.status());
  }

  return zx::ok(fidl::WireSyncClient(std::move(endpoints->client)));
}

zx::result<> Fastboot::SetActive(const std::string& command, Transport* transport) {
  std::vector<std::string_view> args =
      fxl::SplitString(command, ":", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (args.size() < 2) {
    return SendResponse(ResponseType::kFail, "Not enough arguments", transport);
  }

  auto boot_manager_res = FindBootManager();
  if (boot_manager_res.is_error()) {
    return SendResponse(ResponseType::kFail, "Failed to find boot manager", transport,
                        zx::error(boot_manager_res.status_value()));
  }

  fuchsia_paver::wire::Configuration config = fuchsia_paver::wire::Configuration::kB;
  if (args[1] == "a") {
    config = fuchsia_paver::wire::Configuration::kA;
  } else if (args[1] != "b") {
    return SendResponse(ResponseType::kFail, "Invalid slot", transport);
  }

  auto result = boot_manager_res.value()->SetConfigurationActive(config);
  zx_status_t status = result.ok() ? result.value().status : result.status();
  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to set configuration active: ", transport,
                        zx::error(status));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin>>
Fastboot::ConnectToPowerStateControl() {
  auto svc_root = GetSvcRoot();
  if (svc_root.is_error()) {
    return zx::error(svc_root.status_value());
  }

  auto connect_result = component::ConnectAt<fuchsia_hardware_power_statecontrol::Admin>(*svc_root);
  if (connect_result.is_error()) {
    return zx::error(connect_result.status_value());
  }

  return zx::ok(fidl::WireSyncClient(std::move(connect_result.value())));
}

zx::result<> Fastboot::Reboot(const std::string& command, Transport* transport) {
  auto connect_result = ConnectToPowerStateControl();
  if (connect_result.is_error()) {
    return SendResponse(ResponseType::kFail,
                        "Failed to connect to power state control service: ", transport,
                        zx::error(connect_result.status_value()));
  }

  // Send an okay response regardless of the result. Because once system reboots, we have
  // no chance to send any response.
  zx::result<> ret = SendResponse(ResponseType::kOkay, "", transport);
  if (ret.is_error()) {
    return ret;
  }
  // Wait for 1s to make sure the response is sent over to the transport
  std::this_thread::sleep_for(std::chrono::seconds(1));

  fidl::Arena arena;
  auto builder = fuchsia_hardware_power_statecontrol::wire::RebootOptions::Builder(arena);
  fuchsia_hardware_power_statecontrol::RebootReason2 reasons[1] = {
      fuchsia_hardware_power_statecontrol::RebootReason2::kUserRequest};
  auto vector_view =
      fidl::VectorView<fuchsia_hardware_power_statecontrol::RebootReason2>::FromExternal(reasons);
  builder.reasons(vector_view);
  auto resp = connect_result.value()->PerformReboot(builder.Build());
  if (!resp.ok()) {
    return zx::error(resp.status());
  }

  return zx::ok();
}

zx::result<> Fastboot::Continue(const std::string& command, Transport* transport) {
  zx::result<> ret = SendResponse(
      ResponseType::kInfo, "userspace fastboot cannot continue, rebooting instead", transport);
  if (ret.is_error()) {
    return ret;
  }

  return Reboot(command, transport);
}

zx::result<> Fastboot::RebootBootloader(const std::string& command, Transport* transport) {
  zx::result<> ret = SendResponse(
      ResponseType::kInfo,
      "userspace fastboot cannot reboot to bootloader, rebooting to recovery instead", transport);
  if (ret.is_error()) {
    return ret;
  }

  auto connect_result = ConnectToPowerStateControl();
  if (connect_result.is_error()) {
    return SendResponse(ResponseType::kFail,
                        "Failed to connect to power state control service: ", transport,
                        zx::error(connect_result.status_value()));
  }

  // Send an okay response regardless of the result. Because once system reboots, we have
  // no chance to send any response.
  ret = SendResponse(ResponseType::kOkay, "", transport);
  if (ret.is_error()) {
    return ret;
  }
  // Wait for 1s to make sure the response is sent over to the transport
  std::this_thread::sleep_for(std::chrono::seconds(1));

  auto resp = connect_result.value()->RebootToRecovery();
  if (!resp.ok()) {
    return zx::error(resp.status());
  }

  return zx::ok();
}

zx::result<> Fastboot::OemAddStagedBootloaderFile(const std::string& command,
                                                  Transport* transport) {
  std::vector<std::string_view> args =
      fxl::SplitString(command, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (args.size() != 3) {
    return SendResponse(ResponseType::kFail, "Invalid number of arguments", transport);
  }

  if (args[2] != sshd_host::kAuthorizedKeysBootloaderFileName) {
    return SendResponse(ResponseType::kFail, "Unsupported file: " + std::string(args[2]),
                        transport);
  }

  auto svc_root = GetSvcRoot();
  if (svc_root.is_error()) {
    return zx::error(svc_root.status_value());
  }

  auto connect_result = component::ConnectAt<fuchsia_fshost::Admin>(*svc_root);
  if (connect_result.is_error()) {
    return SendResponse(ResponseType::kFail, "Failed to connect to fshost", transport,
                        zx::error(connect_result.status_value()));
  }

  fidl::WireSyncClient fshost_admin{std::move(connect_result.value())};
  auto resp = fshost_admin->WriteDataFile(
      fidl::StringView::FromExternal(sshd_host::kAuthorizedKeyPathInData),
      download_vmo_mapper_.Release());

  zx_status_t status = ZX_OK;
  if (resp.status() != ZX_OK) {
    status = resp.status();
  } else if (resp->is_error()) {
    status = resp->error_value();
  }

  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to write ssh key", transport,
                        zx::error(status));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<fidl::WireSyncClient<fuchsia_paver::DynamicDataSink>> Fastboot::ConnectToDynamicDataSink(
    Transport* transport) {
  auto paver_client_res = ConnectToPaver();
  if (paver_client_res.is_error()) {
    return zx::error(SendResponse(ResponseType::kFail, "Failed to connect to paver", transport,
                                  zx::error(paver_client_res.status_value()))
                         .status_value());
  }

  auto data_sink_endpoints = fidl::CreateEndpoints<fuchsia_paver::DynamicDataSink>();
  if (data_sink_endpoints.is_error()) {
    return zx::error(data_sink_endpoints.status_value());
  }
  auto [data_sink_local, data_sink_remote] = std::move(*data_sink_endpoints);
  auto res = paver_client_res.value()->FindPartitionTableManager(
      fidl::ServerEnd<fuchsia_paver::DynamicDataSink>(data_sink_remote.TakeChannel()));

  if (!res.ok()) {
    return zx::error(SendResponse(ResponseType::kFail, "Failed to find dynamic data sink",
                                  transport, zx::error(res.status()))
                         .status_value());
  }

  return zx::ok(fidl::WireSyncClient(std::move(data_sink_local)));
}

zx::result<> Fastboot::OemInitPartitionTables(const std::string& command, Transport* transport) {
  auto data_sink_client = ConnectToDynamicDataSink(transport);
  if (data_sink_client.is_error()) {
    return zx::error(data_sink_client.status_value());
  }

  auto res = data_sink_client.value()->InitializePartitionTables();
  auto status = res.ok() ? res.value().status : res.status();
  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to init partition table", transport,
                        zx::error(status));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::OemInstallFromUsb(const std::string& command, Transport* transport) {
  // Command format: oem install-from-usb [source [destination]].
  // Source and/or destination can be the string "default" in order to use the default target.
  //
  // Note: source/destination aren't really usable at the moment because the path names will
  // often be too large to fit in the fastboot packet. Example paths from a NUC11:
  //   * "/dev/sys/platform/pt/PC00/bus/00:14.0/00:14.0/xhci/usb-bus/001/001/ifc-000/ums/scsi-block-device-0-0/block"
  //   * "/dev/sys/platform/pt/PC00/bus/01:00.0/01:00.0/nvme/namespace-1/block"
  // We'll need to implement some sort of substring matching to make this actually useful.
  // Probably also a way to list the current disks so the user doesn't have to magically know this
  // entire string.
  std::vector<std::string_view> args =
      fxl::SplitString(command, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);

  // Make a copy of any arg we need to pass because they must be null-terminated c-strings.
  std::string source, dest;
  if (args.size() > 2 && args[2] != "default") {
    source = args[2];
  }
  if (args.size() > 3 && args[3] != "default") {
    dest = args[3];
  }

  zx_status_t status = install_from_usb(source.empty() ? nullptr : source.c_str(),
                                        dest.empty() ? nullptr : dest.c_str());
  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to install from USB", transport,
                        zx::error(status));
  }
  return SendResponse(ResponseType::kOkay, "", transport);
}

zx::result<> Fastboot::OemWipePartitionTables(const std::string& command, Transport* transport) {
  auto data_sink_client = ConnectToDynamicDataSink(transport);
  if (data_sink_client.is_error()) {
    return zx::error(data_sink_client.status_value());
  }

  auto res = data_sink_client.value()->WipePartitionTables();
  auto status = res.ok() ? res.value().status : res.status();
  if (status != ZX_OK) {
    return SendResponse(ResponseType::kFail, "Failed to wipe partition table", transport,
                        zx::error(status));
  }

  return SendResponse(ResponseType::kOkay, "", transport);
}

}  // namespace fastboot
