// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_USB_RS_IOKIT_USB_STUB_H_
#define SRC_LIB_USB_RS_IOKIT_USB_STUB_H_
#ifdef __APPLE__

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/IOMessage.h>
#include <IOKit/usb/IOUSBLib.h>
#include <mach/mach_port.h>

CFUUIDRef GetIOUSBDeviceUserClientTypeID(void);

CFUUIDRef GetIOUSBInterfaceUserClientTypeID(void);

CFUUIDRef GetIOCFPlugInInterfaceID(void);

CFUUIDRef GetIOUSBDeviceInterfaceID500(void);

CFUUIDRef GetIOUSBInterfaceInterfaceID500(void);

#endif  // __APPLE__
#endif  // SRC_LIB_USB_RS_IOKIT_USB_STUB_H_
