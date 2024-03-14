// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// ADDING A NEW PROTOCOL
// When adding a new protocol, add a macro call at the end of this file after
// the last protocol definition with a tag, value, name, and flags in the form:
//
// DDK_PROTOCOL_DEF(tag, value, protocol_name)
//
// The value must be a unique identifier that is just the previous protocol
// value plus 1.

// clang-format off

#ifndef DDK_FIDL_PROTOCOL_DEF
#error Internal use only. Do not include.
#else
// 1 was "fuchsia.hardware.rpmb.Service"
// 2 was "fuchsia.hardware.google.ec.Service"
// 3 was "fuchsia.hardware.i2c.Service"
DDK_FIDL_PROTOCOL_DEF(PCI,             4, "fuchsia.hardware.pci.Service")
// 5 was "fuchsia.hardware.goldfish.pipe.Service"
// 6 was "fuchsia.hardware.goldfish.AddressSpaceService"
// 7 was "fuchsia.hardware.goldfish.SyncService"
// 8 was "fuchsia.hardware.spi.Service"
DDK_FIDL_PROTOCOL_DEF(SYSMEM,          9, "fuchsia.hardware.sysmem.Service")
// 10 was "fuchsia.hardware.mailbox.Service"
DDK_FIDL_PROTOCOL_DEF(PLATFORM_BUS,    11, "fuchsia.hardware.platform.bus.PlatformBus")
// 12 was "fuchsia.hardware.interrupt.Provider"
// 13 was "fuchsia.hardware.platform.device.Service"
// 14 was "fuchsia.hardware.dsp.Service"
// 15 was "fuchsia.hardware.hdmi.Service"
// 16 was "fuchsia.hardware.power.sensor.Service"
// 17 was "fuchsia.hardware.vreg.Service"
// 18 was "fuchsia.hardware.registers.Service"
// 19 was "fuchsia.hardware.tee.Service"
// 20 was "fuchsia.hardware.amlogiccanvas.Service"
// 21 was "fuchsia.hardware.power.Service"
// 22 was "fuchsia.hardware.clock.Service"
// 23 was "fuchsia.hardware.audio.CodecService"
DDK_FIDL_PROTOCOL_DEF(PWM,             24, "fuchsia.hardware.pwm.Service")
// 25 was "fuchsia.hardware.ethernet.board.Service"
// 26 was "fuchsia.hardware.gpio.Service"
// 27 was "fuchsia.hardware.i2cimpl.Service"
#undef DDK_FIDL_PROTOCOL_DEF
#endif
