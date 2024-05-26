// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fuchsia/hardware/bt/hci/c/banjo.h>
#include <fuchsia/hardware/usb/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zx/vmo.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <future>

#include <fbl/string_printf.h>

#include "firmware_loader.h"
#include "logging.h"

namespace btintel {

// USB Product IDs that use the "secure" firmware method.
static constexpr uint16_t sfi_product_ids[] = {
    0x0025,  // Thunder Peak (9160/9260)
    0x0a2b,  // Snowfield Peak (8260)
    0x0aaa,  // Jefferson Peak (9460/9560)
    0x0026,  // Harrison Peak (AX201)
    0x0032,  // Sun Peak (AX210)
};

// USB Product IDs that use the "legacy" firmware loading method.
static constexpr uint16_t legacy_firmware_loading_ids[] = {
    0x0025,  // Thunder Peak (9160/9260)
    0x0a2b,  // Snowfield Peak (8260)
    0x0aaa,  // Jefferson Peak (9460/9560)
    0x0026,  // Harrison Peak (AX201)
};

Device::Device(zx_device_t* device, bt_hci_protocol_t* hci, bool secure,
               bool legacy_firmware_loading)
    : DeviceType(device),
      devfs_connector_(fit::bind_member<&Device::Connect>(this)),
      hci_(hci),
      secure_(secure),
      firmware_loaded_(false),
      legacy_firmware_loading_(legacy_firmware_loading) {}

zx_status_t Device::bt_intel_bind(void* ctx, zx_device_t* device) {
  tracef("bind\n");

  usb_protocol_t usb;
  zx_status_t result = device_get_protocol(device, ZX_PROTOCOL_USB, &usb);
  if (result != ZX_OK) {
    errorf("couldn't get USB protocol: %s\n", zx_status_get_string(result));
    return result;
  }

  usb_device_descriptor_t dev_desc;
  usb_get_device_descriptor(&usb, &dev_desc);

  // Whether this device uses the "secure" firmware method.
  bool secure = false;
  for (uint16_t id : sfi_product_ids) {
    if (dev_desc.id_product == id) {
      secure = true;
      break;
    }
  }

  // Whether this device uses the "legacy" firmware loading method.
  bool legacy_firmware_loading = false;
  for (uint16_t id : legacy_firmware_loading_ids) {
    if (dev_desc.id_product == id) {
      legacy_firmware_loading = true;
      break;
    }
  }

  bt_hci_protocol_t hci;
  result = device_get_protocol(device, ZX_PROTOCOL_BT_HCI, &hci);
  if (result != ZX_OK) {
    errorf("couldn't get BT_HCI protocol: %s\n", zx_status_get_string(result));
    return result;
  }

  auto btdev = new btintel::Device(device, &hci, secure, legacy_firmware_loading);
  result = btdev->Bind();
  if (result != ZX_OK) {
    errorf("failed binding device: %s\n", zx_status_get_string(result));
    delete btdev;
    return result;
  }
  // Bind succeeded and devmgr is now responsible for releasing |btdev|
  // The device's init hook will load the firmware.
  return ZX_OK;
}

zx_status_t Device::Bind() {
  ddk::DeviceAddArgs args("bt-hci-intel");
  args.set_flags(DEVICE_ADD_NON_BINDABLE);
  return DdkAdd(args);
}

void Device::DdkInit(ddk::InitTxn init_txn) {
  init_thread_ = std::thread(
      [this, txn = std::move(init_txn)]() mutable { LoadFirmware(std::move(txn), secure_); });
}

zx_status_t Device::LoadFirmware(ddk::InitTxn init_txn, bool secure) {
  infof("LoadFirmware(secure: %s, firmware_loading: %s)", (secure ? "yes" : "no"),
        (legacy_firmware_loading_ ? "legacy" : "new"));

  // TODO(armansito): Track metrics for initialization failures.

  zx_status_t status;
  zx::channel our_cmd, their_cmd, our_acl, their_acl;
  status = zx::channel::create(0, &our_cmd, &their_cmd);
  if (status != ZX_OK) {
    return InitFailed(std::move(init_txn), status, "failed to create command channel");
  }
  status = zx::channel::create(0, &our_acl, &their_acl);
  if (status != ZX_OK) {
    return InitFailed(std::move(init_txn), status, "failed to create ACL channel");
  }

  // Get the channels
  status = BtHciOpenCommandChannel(std::move(their_cmd));
  if (status != ZX_OK) {
    return InitFailed(std::move(init_txn), status, "failed to bind command channel");
  }

  status = BtHciOpenAclDataChannel(std::move(their_acl));
  if (status != ZX_OK) {
    return InitFailed(std::move(init_txn), status, "failed to bind ACL channel");
  }

  if (secure) {
    status = LoadSecureFirmware(&our_cmd, &our_acl);
  } else {
    status = LoadLegacyFirmware(&our_cmd, &our_acl);
  }

  if (status != ZX_OK) {
    return InitFailed(std::move(init_txn), status, "failed to initialize controller");
  }

  firmware_loaded_ = true;
  init_txn.Reply(ZX_OK);  // This will make the device visible and able to be unbound.
  return ZX_OK;
}

zx_status_t Device::InitFailed(ddk::InitTxn init_txn, zx_status_t status, const char* note) {
  errorf("%s: %s", note, zx_status_get_string(status));
  init_txn.Reply(status);
  return status;
}

zx_handle_t Device::MapFirmware(const char* name, uintptr_t* fw_addr, size_t* fw_size) {
  zx_handle_t vmo = ZX_HANDLE_INVALID;
  size_t size;
  zx_status_t status = load_firmware(zxdev(), name, &vmo, &size);
  if (status != ZX_OK) {
    errorf("failed to load firmware '%s': %s", name, zx_status_get_string(status));
    return ZX_HANDLE_INVALID;
  }
  status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, size, fw_addr);
  if (status != ZX_OK) {
    errorf("firmware map failed: %s", zx_status_get_string(status));
    return ZX_HANDLE_INVALID;
  }
  *fw_size = size;
  return vmo;
}

void Device::DdkUnbind(ddk::UnbindTxn txn) {
  tracef("unbind");
  txn.Reply();
}

void Device::DdkRelease() {
  if (init_thread_.joinable()) {
    init_thread_.join();
  }
  delete this;
}

zx_status_t Device::DdkGetProtocol(uint32_t proto_id, void* out_proto) {
  if (proto_id != ZX_PROTOCOL_BT_HCI) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bt_hci_protocol_t* hci_proto = static_cast<bt_hci_protocol_t*>(out_proto);

  // Forward the underlying bt-transport ops.
  hci_.GetProto(hci_proto);

  return ZX_OK;
}

void Device::OpenCommandChannel(OpenCommandChannelRequestView request,
                                OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = BtHciOpenCommandChannel(std::move(request->channel)); status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}
void Device::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = BtHciOpenAclDataChannel(std::move(request->channel)); status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}
void Device::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                OpenScoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void Device::ConfigureSco(ConfigureScoRequestView request, ConfigureScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void Device::ResetSco(ResetScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void Device::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                OpenIsoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void Device::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                              OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = BtHciOpenSnoopChannel(std::move(request->channel)); status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}
void Device::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method in Hci request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void Device::EncodeCommand(EncodeCommandRequestView request,
                           EncodeCommandCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void Device::OpenHci(OpenHciCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }
  hci_binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                std::move(endpoints->server), this, fidl::kIgnoreBindingClosure);

  completer.ReplySuccess(std::move(endpoints->client));
}
void Device::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method in Vendor request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

// driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor>
void Device::Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request) {
  vendor_binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   std::move(request), this, fidl::kIgnoreBindingClosure);

  vendor_binding_group_.ForEachBinding(
      [](const fidl::ServerBinding<fuchsia_hardware_bluetooth::Vendor>& binding) {
        fidl::Arena arena;
        auto builder = fuchsia_hardware_bluetooth::wire::VendorFeatures::Builder(arena);
        builder.acl_priority_command(false);
        fidl::Status status = fidl::WireSendEvent(binding)->OnFeatures(builder.Build());

        if (status.status() != ZX_OK) {
          zxlogf(ERROR, "Failed to send vendor features to bt-host: %s", status.status_string());
        }
      });
}

zx_status_t Device::LoadSecureFirmware(zx::channel* cmd, zx::channel* acl) {
  ZX_DEBUG_ASSERT(cmd);
  ZX_DEBUG_ASSERT(acl);

  VendorHci hci(cmd);

  // Bring the controller to a well-defined default state.
  // Send an initial reset. If the controller sends a "command not supported"
  // event on newer models, this likely means that the controller is in
  // bootloader mode and we can ignore the error.
  auto hci_status = hci.SendHciReset();
  if (hci_status == pw::bluetooth::emboss::StatusCode::UNKNOWN_COMMAND) {
    infof("Ignoring \"Unknown Command\" error while in bootloader mode");
  } else if (hci_status != pw::bluetooth::emboss::StatusCode::SUCCESS) {
    errorf("HCI_Reset failed (status: 0x%02hhx)", static_cast<unsigned char>(hci_status));
    return ZX_ERR_BAD_STATE;
  }

  // Newer Intel controllers that use the "secure send" method can send HCI
  // events over the bulk endpoint. Enable this before sending the initial
  // ReadVersion command.
  hci.enable_events_on_bulk(acl);

  btintel::SecureBootEngineType engine_type = btintel::SecureBootEngineType::kRSA;

  fbl::String fw_filename;
  if (legacy_firmware_loading_) {
    ReadVersionReturnParams version = hci.SendReadVersion();
    if (version.status != pw::bluetooth::emboss::StatusCode::SUCCESS) {
      errorf("failed to obtain version information");
      return ZX_ERR_BAD_STATE;
    }

    // If we're already in firmware mode, we're done.
    if (version.fw_variant == kFirmwareFirmwareVariant) {
      infof("firmware loaded (variant %d, revision %d)", version.hw_variant, version.hw_revision);
      return ZX_OK;
    }

    // If we reached here then the controller must be in bootloader mode.
    if (version.fw_variant != kBootloaderFirmwareVariant) {
      errorf("unsupported firmware variant (0x%x)", version.fw_variant);
      return ZX_ERR_NOT_SUPPORTED;
    }

    ReadBootParamsReturnParams boot_params = hci.SendReadBootParams();
    if (boot_params.status != pw::bluetooth::emboss::StatusCode::SUCCESS) {
      errorf("failed to read boot parameters");
      return ZX_ERR_BAD_STATE;
    }

    // Map the firmware file into memory.
    fw_filename = fbl::StringPrintf("ibt-%d-%d.sfi", version.hw_variant, boot_params.dev_revid);
  } else {
    ReadVersionReturnParamsTlv version = hci.SendReadVersionTlv();

    engine_type = (version.secure_boot_engine_type == 0x01) ? btintel::SecureBootEngineType::kECDSA
                                                            : btintel::SecureBootEngineType::kRSA;

    // If we're already in firmware mode, we're done.
    if (version.current_mode_of_operation == kCurrentModeOfOperationOperationalFirmware) {
      infof("firmware already loaded");
      return ZX_OK;
    }

    fw_filename = fbl::StringPrintf("ibt-%04x-%04x.sfi", 0x0041, 0x0041);
  }

  zx::vmo fw_vmo;
  uintptr_t fw_addr;
  size_t fw_size;
  fw_vmo.reset(MapFirmware(fw_filename.c_str(), &fw_addr, &fw_size));
  if (!fw_vmo) {
    errorf("failed to map firmware");
    return ZX_ERR_NOT_SUPPORTED;
  }

  FirmwareLoader loader(cmd, acl);

  // The boot addr differs for different firmware.  Save it for later.
  uint32_t boot_addr = 0x00000000;
  auto status = loader.LoadSfi(reinterpret_cast<void*>(fw_addr), fw_size, engine_type, &boot_addr);
  zx_vmar_unmap(zx_vmar_root_self(), fw_addr, fw_size);
  if (status == FirmwareLoader::LoadStatus::kError) {
    errorf("failed to load SFI firmware");
    return ZX_ERR_BAD_STATE;
  }

  // Switch the controller into firmware mode.
  hci.SendVendorReset(boot_addr);

  infof("firmware loaded using %s", fw_filename.c_str());
  return ZX_OK;
}

zx_status_t Device::LoadLegacyFirmware(zx::channel* cmd, zx::channel* acl) {
  ZX_DEBUG_ASSERT(cmd);
  ZX_DEBUG_ASSERT(acl);

  VendorHci hci(cmd);

  // Bring the controller to a well-defined default state.
  auto hci_status = hci.SendHciReset();
  if (hci_status != pw::bluetooth::emboss::StatusCode::SUCCESS) {
    errorf("HCI_Reset failed (status: 0x%02hhx)", static_cast<unsigned char>(hci_status));
    return ZX_ERR_BAD_STATE;
  }

  ReadVersionReturnParams version = hci.SendReadVersion();
  if (version.status != pw::bluetooth::emboss::StatusCode::SUCCESS) {
    errorf("failed to obtain version information");
    return ZX_ERR_BAD_STATE;
  }

  if (version.fw_patch_num > 0) {
    infof("controller already patched");
    return ZX_OK;
  }

  auto fw_filename = fbl::StringPrintf(
      "ibt-hw-%x.%x.%x-fw-%x.%x.%x.%x.%x.bseq", version.hw_platform, version.hw_variant,
      version.hw_revision, version.fw_variant, version.fw_revision, version.fw_build_num,
      version.fw_build_week, version.fw_build_year);

  zx::vmo fw_vmo;
  uintptr_t fw_addr;
  size_t fw_size;
  fw_vmo.reset(MapFirmware(fw_filename.c_str(), &fw_addr, &fw_size));

  // Try the fallback patch file on initial failure.
  if (!fw_vmo) {
    // Try the fallback patch file
    fw_filename = fbl::StringPrintf("ibt-hw-%x.%x.bseq", version.hw_platform, version.hw_variant);
    fw_vmo.reset(MapFirmware(fw_filename.c_str(), &fw_addr, &fw_size));
  }

  // Abort if the fallback file failed to load too.
  if (!fw_vmo) {
    errorf("failed to map firmware");
    return ZX_ERR_NOT_SUPPORTED;
  }

  FirmwareLoader loader(cmd, acl);
  hci.EnterManufacturerMode();
  auto result = loader.LoadBseq(reinterpret_cast<void*>(fw_addr), fw_size);
  hci.ExitManufacturerMode(result == FirmwareLoader::LoadStatus::kPatched
                               ? MfgDisableMode::kPatchesEnabled
                               : MfgDisableMode::kNoPatches);
  zx_vmar_unmap(zx_vmar_root_self(), fw_addr, fw_size);

  if (result == FirmwareLoader::LoadStatus::kError) {
    errorf("failed to patch controller");
    return ZX_ERR_BAD_STATE;
  }

  infof("controller patched using %s", fw_filename.c_str());
  return ZX_OK;
}

zx_status_t Device::BtHciOpenCommandChannel(zx::channel in) {
  return hci_.OpenCommandChannel(std::move(in));
}

zx_status_t Device::BtHciOpenAclDataChannel(zx::channel in) {
  return hci_.OpenAclDataChannel(std::move(in));
}

zx_status_t Device::BtHciOpenScoChannel(zx::channel in) {
  return hci_.OpenScoChannel(std::move(in));
}

void Device::BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                               sco_sample_rate_t sample_rate,
                               bt_hci_configure_sco_callback callback, void* cookie) {
  hci_.ConfigureSco(coding_format, encoding, sample_rate, callback, cookie);
}

void Device::BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie) {
  hci_.ResetSco(callback, cookie);
}

zx_status_t Device::BtHciOpenIsoDataChannel(zx::channel in) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Device::BtHciOpenSnoopChannel(zx::channel in) {
  return hci_.OpenSnoopChannel(std::move(in));
}

static constexpr zx_driver_ops_t btintel_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = btintel::Device::bt_intel_bind;
  return ops;
}();

}  // namespace btintel

ZIRCON_DRIVER(bt_hci_intel, btintel::btintel_driver_ops, "fuchsia", "0.1");
