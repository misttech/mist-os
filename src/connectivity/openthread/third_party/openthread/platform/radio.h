// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/**
 * @file
 *   This file include declarations of functions that will be called in fuchsia component.
 */

#ifndef SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_RADIO_H_
#define SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_RADIO_H_

#include "openthread-system.h"
#include "radio_url.h"
#include "spinel_fidl_interface.h"

#define OT_CORE_COMMON_NEW_HPP_  // Use the new operator defined in fuchsia instead
#include <spinel/radio_spinel.hpp>
#undef OT_CORE_COMMON_NEW_HPP_

#define OPENTHREAD_POSIX_CONFIG_SPINEL_VENDOR_INTERFACE_ENABLE 1

extern "C" otError otPlatRadioEnable(otInstance *a_instance);
extern "C" otInstance *otPlatRadioCheckOtInstanceEnabled();
extern "C" void platformRadioProcess(otInstance *aInstance, const otSysMainloopContext *aContext);

namespace ot {
namespace Posix {
typedef ot::Fuchsia::SpinelFidlInterface VendorInterface;

/**
 * Manages Thread radio.
 *
 */
class Radio {
 public:
  /**
   * Creates the radio manager.
   *
   */
  Radio(void);

  /**
   * Initialize the Thread radio.
   *
   * @param[in]   aUrl    A pointer to the null-terminated URL.
   *
   */
  void Init(const char *aUrl);

  /**
   * Acts as an accessor to the spinel interface instance used by the radio.
   *
   * @returns A reference to the radio's spinel interface instance.
   *
   */
  Spinel::SpinelInterface &GetSpinelInterface(void) {
    OT_ASSERT(mSpinelInterface != nullptr);
    return *mSpinelInterface;
  }

  /**
   * Acts as an accessor to the radio spinel instance used by the radio.
   *
   * @returns A reference to the radio spinel instance.
   *
   */
  Spinel::RadioSpinel &GetRadioSpinel(void) { return mRadioSpinel; }

 private:
#if OPENTHREAD_POSIX_VIRTUAL_TIME
  void VirtualTimeInit(void);
#endif
  void ProcessRadioUrl(const RadioUrl &aRadioUrl);
  void ProcessMaxPowerTable(const RadioUrl &aRadioUrl);

  Spinel::SpinelInterface *CreateSpinelInterface(const char *aInterfaceName);
  void GetIidListFromRadioUrl(spinel_iid_t (&aIidList)[Spinel::kSpinelHeaderMaxNumIid]);

#if OPENTHREAD_POSIX_CONFIG_SPINEL_HDLC_INTERFACE_ENABLE && \
    OPENTHREAD_POSIX_CONFIG_SPINEL_SPI_INTERFACE_ENABLE
  static constexpr size_t kSpinelInterfaceRawSize = sizeof(ot::Posix::SpiInterface) >
                                                            sizeof(ot::Posix::HdlcInterface)
                                                        ? sizeof(ot::Posix::SpiInterface)
                                                        : sizeof(ot::Posix::HdlcInterface);
#elif OPENTHREAD_POSIX_CONFIG_SPINEL_HDLC_INTERFACE_ENABLE
  static constexpr size_t kSpinelInterfaceRawSize = sizeof(ot::Posix::HdlcInterface);
#elif OPENTHREAD_POSIX_CONFIG_SPINEL_SPI_INTERFACE_ENABLE
  static constexpr size_t kSpinelInterfaceRawSize = sizeof(ot::Posix::SpiInterface);
#elif OPENTHREAD_POSIX_CONFIG_SPINEL_VENDOR_INTERFACE_ENABLE
  static constexpr size_t kSpinelInterfaceRawSize = sizeof(ot::Posix::VendorInterface);
#else
#error "No Spinel interface is specified!"
#endif

  RadioUrl mRadioUrl;
#if OPENTHREAD_SPINEL_CONFIG_VENDOR_HOOK_ENABLE
  Spinel::VendorRadioSpinel mRadioSpinel;
#else
  Spinel::RadioSpinel mRadioSpinel;
#endif
  Spinel::SpinelInterface *mSpinelInterface;

  OT_DEFINE_ALIGNED_VAR(mSpinelInterfaceRaw, kSpinelInterfaceRawSize, uint64_t);
};

}  // namespace Posix
}  // namespace ot

#endif  // SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_RADIO_H_
