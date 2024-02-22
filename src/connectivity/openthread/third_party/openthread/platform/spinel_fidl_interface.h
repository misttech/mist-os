// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_SPINEL_FIDL_INTERFACE_H_
#define SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_SPINEL_FIDL_INTERFACE_H_

#include <stdint.h>
#include <string.h>

#include <openthread/error.h>
#include <spinel/spinel.h>

#include "openthread-system.h"
#include "radio_url.h"

#include <spinel/spinel_interface.hpp>

namespace ot {
namespace Fuchsia {

void spinelInterfaceInit(otInstance* a_instance);
/**
 * This class defines an spinel interface to the Radio Co-processor (RCP)
 *
 */
#ifdef OPENTHREAD_231010
class SpinelFidlInterface {
#endif
#ifdef OPENTHREAD_240214
  class SpinelFidlInterface : public ot::Spinel::SpinelInterface {
#endif
   public:
    /**
     * Constructor
     *
     */
#ifdef OPENTHREAD_231010
    SpinelFidlInterface(Spinel::SpinelInterface::ReceiveFrameCallback aCallback,
                        void* aCallbackContext,
                        Spinel::SpinelInterface::RxFrameBuffer& aFrameBuffer);
#endif
#ifdef OPENTHREAD_240214
    SpinelFidlInterface(const Url::Url& aRadioUrl);
#endif
    /**
     * Initializes the Spinel Fidl Interface
     *
     */
#ifdef OPENTHREAD_231010
    otError Init(void);
#endif
#ifdef OPENTHREAD_240214
    otError Init(ReceiveFrameCallback aCallback, void* aCallbackContext,
                 RxFrameBuffer& aFrameBuffer);
#endif
    /**
     * Deinitialized the instance
     *
     */

    void Deinit(void);
    /**
     * Send the frame from ot-lib to ot-radio driver
     *
     */
#ifdef OPENTHREAD_231010
    otError SendFrame(uint8_t* aFrame, uint16_t aLength);
#endif
#ifdef OPENTHREAD_240214
    otError SendFrame(const uint8_t* aFrame, uint16_t aLength);
#endif
    /**
     * Used for waiting for a spinel frame response with a timeout
     *
     */
    otError WaitForFrame(uint64_t aTimeoutUs);
    /**
     * Used for process a spinel frame event
     *
     */
    void Process(const void* aContext);

    /**
     * This method is called when RCP failure detected and resets internal states of the interface.
     *
     */
    void OnRcpReset(void);

    /**
     * This method is called when RCP is reset to recreate the connection with it.
     *
     */
    otError ResetConnection(void);

    /**
     * This method hardware resets the RCP.
     *
     * @retval OT_ERROR_NONE            Successfully reset the RCP.
     * @retval OT_ERROR_NOT_IMPLEMENT   The hardware reset is not implemented.
     *
     */
    otError HardwareReset(void) { return OT_ERROR_NOT_IMPLEMENTED; }

#ifdef OPENTHREAD_240214
    /**
     * Returns the RCP interface metrics.
     *
     * @returns The RCP interface metrics.
     *
     */
    const otRcpInterfaceMetrics* GetRcpInterfaceMetrics(void) const { return &mInterfaceMetrics; }

    void UpdateFdSet(void* aMainloopContext) {}

    uint32_t GetBusSpeed(void) const { return 0; }

    static bool IsInterfaceNameMatch(const char* aInterfaceName) { return true; }
#endif

   private:
    /**
     * Write received inbound frame to the buffer where can be processed by ot-lib
     *
     */
    void WriteToRxFrameBuffer(std::vector<uint8_t> vec);
#ifdef OPENTHREAD_231010
    Spinel::SpinelInterface::ReceiveFrameCallback mReceiveFrameCallback;
    void* mReceiveFrameContext;
    Spinel::SpinelInterface::RxFrameBuffer& mReceiveFrameBuffer;
#endif
#ifdef OPENTHREAD_240214
    ReceiveFrameCallback mReceiveFrameCallback;
    void* mReceiveFrameContext;
    RxFrameBuffer* mRxFrameBuffer;

    otRcpInterfaceMetrics mInterfaceMetrics;
    [[maybe_unused]]
    const Url::Url& mRadioUrl;
#endif
  };  // SpinelFidlInterface
}  // namespace Fuchsia
}  // namespace ot

#endif  // SRC_CONNECTIVITY_OPENTHREAD_THIRD_PARTY_OPENTHREAD_PLATFORM_SPINEL_FIDL_INTERFACE_H_
