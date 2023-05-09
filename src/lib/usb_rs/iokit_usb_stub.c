// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "iokit_usb_stub.h"

CFUUIDRef GetIOUSBDeviceUserClientTypeID(void) { return kIOUSBDeviceUserClientTypeID; }

CFUUIDRef GetIOUSBInterfaceUserClientTypeID(void) { return kIOUSBInterfaceUserClientTypeID; }

CFUUIDRef GetIOCFPlugInInterfaceID(void) { return kIOCFPlugInInterfaceID; }

CFUUIDRef GetIOUSBDeviceInterfaceID500(void) { return kIOUSBDeviceInterfaceID500; }

CFUUIDRef GetIOUSBInterfaceInterfaceID500(void) { return kIOUSBInterfaceInterfaceID500; }
