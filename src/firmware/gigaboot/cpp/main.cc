// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "phys/efi/main.h"

#include <lib/abr/abr.h>
#include <stdio.h>

#include <phys/stdio.h>

#include "backends.h"
#include "fastboot_tcp.h"
#include "gigaboot/src/netifc.h"
#include "input.h"
#include "lib/zircon_boot/zircon_boot.h"
#include "utils.h"
#include "xefi.h"
#include "zircon_boot_ops.h"

#if defined(__x86_64__)
#include <cpuid.h>
#endif

// This is necessary because the Fastboot class inherits from FastbootBase,
// which is an abstract class with pure virtual functions.
// The _purecall definition is not provided in efi compilation, which leads to an
// 'undefined' error in asan builds.
extern "C" int _purecall(void) { return 0; }

namespace {
// Check for KVM or non-KVM QEMU using Hypervisor Vendor ID.
bool IsQemu() {
#if defined(__x86_64__)
  uint32_t eax;
  uint32_t name[3];
  __cpuid(0x40000000, eax, name[0], name[1], name[2]);
  std::string_view name_str(reinterpret_cast<const char*>(name), sizeof(name));
  return name_str == "TCGTCGTCGTCG"sv || name_str == "KVMKVMKVM\0\0\0"sv;
#else
  return false;
#endif
}

// Always enable serial output if not already. Infra relies on serial to get device feedback.
// Without it, some CI/CQs fail.
void SetSerial() {
  PhysConsole& console = PhysConsole::Get();
  if (*console.serial() != FILE()) {
    return;
  }

  // QEMU routes serial output to the console. To avoid double printing, avoid using serial
  // output when running in QEMU.
  if (IsQemu()) {
    return;
  }

  // Temporarily set `gEfiSystemTable->ConOut` to NULL to force serial console setup.
  // This won't affect the graphic set up done earlier.
  //
  // This function can be removed once SetEfiStdout() can correctly set up both graphics and serial
  // output.

  auto conn_out = gEfiSystemTable->ConOut;
  gEfiSystemTable->ConOut = nullptr;
  SetEfiStdout(gEfiSystemTable);
  gEfiSystemTable->ConOut = conn_out;
}
}  // namespace

// TODO(b/285053546) 'BootByte' usage should be removed in favour of ABR Metadata
bool ResetRebootMode(gigaboot::RebootMode reboot_mode, const AbrOps& abr_ops) {
  if (reboot_mode != gigaboot::RebootMode::kNormal &&
      !SetRebootMode(gigaboot::RebootMode::kNormal)) {
    printf("Failed to reset reboot mode\n");
    return false;
  }

  return true;
}

int main(int argc, char** argv) {
  SetSerial();
  printf("Gigaboot main\n");

  auto is_secureboot_on = gigaboot::IsSecureBootOn();
  if (is_secureboot_on.is_error()) {
    printf("Failed to query SecureBoot variable\n");
  } else {
    printf("Secure Boot: %s\n", *is_secureboot_on ? "On" : "Off");
  }

  // TODO(b/235489025): We reuse some legacy C gigaboot code for stuff like network stack.
  // This initializes the global variables the legacy code needs. Once these needed features are
  // re-implemented, remove these dependencies.
  xefi_init(gEfiImageHandle, gEfiSystemTable);
  // The following check/initialize network interface and generate ip6 address.
  if (netifc_open()) {
    printf("netifc: Failed to open network interface\n");
  } else {
    printf("netifc: network interface opened\n");
  }

  // Log TPM info if the device has one.
  if (efi_status res = gigaboot::PrintTpm2Capability(); res != EFI_SUCCESS) {
    printf("Failed to log TPM 2.0 capability %s. TPM 2.0 may not be supported\n",
           gigaboot::EfiStatusToString(res));
  }

  // Print OneShotFlags from ABR
  AbrDataOneShotFlags one_shot_flags;
  ZirconBootOps zb_ops = gigaboot::GetZirconBootOps();
  AbrOps abr_ops = GetAbrOpsFromZirconBootOps(&zb_ops);
  AbrResult abr_res = AbrGetAndClearOneShotFlags(&abr_ops, &one_shot_flags);
  if (abr_res != kAbrResultOk) {
    printf("Failed to get one shot flags from ABR\n");
    return 1;
  }
  printf("abr.one_shot_flags = 0x%02x\n", one_shot_flags);

  gigaboot::RebootMode reboot_mode =
      gigaboot::GetRebootMode(one_shot_flags).value_or(gigaboot::RebootMode::kNormal);

  // TODO(b/285053546) 'BootByte' usage should be removed in favour of ABR Metadata
  // Reset previous reboot mode immediately to prevent it from being sticky.
  if (!ResetRebootMode(reboot_mode, abr_ops)) {
    printf("Failed to reset reboot mode\n");
    return 1;
  }

  bool enter_fastboot = reboot_mode == gigaboot::RebootMode::kBootloader;
  if (enter_fastboot) {
    printf(
        "Your BIOS or ABR instructed Gigaboot to enter fastboot directly and skip normal boot.\n");
  } else {
    constexpr zx::duration timeout = zx::sec(2);
    gigaboot::InputReceiver receiver(gEfiSystemTable);
    printf("Press f to enter fastboot.\n");
    std::optional<char> key = receiver.GetKeyPrompt("f", timeout, "Auto boot in");
    enter_fastboot = key == 'f';
  }

  if (enter_fastboot) {
    zx::result<> ret = gigaboot::FastbootTcpMain();
    if (ret.is_error()) {
      printf("Fastboot failed\n");
      return 1;
    }
  }

  ZirconBootMode boot_mode = reboot_mode == gigaboot::RebootMode::kRecovery
                                 ? kZirconBootModeForceRecovery
                                 : kZirconBootModeAbr;

  // TODO(b/236039205): Implement logic to construct these arguments for the API. This
  // is currently a placeholder for testing compilation/linking.
  ZirconBootOps zircon_boot_ops = gigaboot::GetZirconBootOps();
  ZirconBootResult res = LoadAndBoot(&zircon_boot_ops, boot_mode);
  if (res != kBootResultOK) {
    printf("Failed to boot zircon\n");
    return 1;
  }

  return 0;
}
