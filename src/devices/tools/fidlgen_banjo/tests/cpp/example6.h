// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.example6 banjo file

#pragma once

#include <banjo/examples/example6/c/banjo.h>
#include <ddktl/device-internal.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/env.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include "banjo-internal.h"

// DDK example6-protocol support
//
// :: Proxies ::
//
// ddk::HelloProtocolClient is a simple wrapper around
// hello_protocol_t. It does not own the pointers passed to it.
//
// :: Mixins ::
//
// ddk::HelloProtocol is a mixin class that simplifies writing DDK drivers
// that implement the hello protocol. It doesn't set the base protocol.
//
// :: Examples ::
//
// // A driver that implements a ZX_PROTOCOL_HELLO device.
// class HelloDevice;
// using HelloDeviceType = ddk::Device<HelloDevice, /* ddk mixins */>;
//
// class HelloDevice : public HelloDeviceType,
//                      public ddk::HelloProtocol<HelloDevice> {
//   public:
//     HelloDevice(zx_device_t* parent)
//         : HelloDeviceType(parent) {}
//
//     void HelloSay(const char* req, char* out_response, size_t response_capacity);
//
//     ...
// };

namespace ddk {

template <typename D, typename Base = internal::base_mixin, bool runtime_enforce_no_reentrancy = false>
class HelloProtocol : public Base {
public:
    HelloProtocol() {
        hello_protocol_server_driver_ = fdf_env_get_current_driver();
        internal::CheckHelloProtocolSubclass<D>();
        hello_protocol_ops_.say = HelloSay;

        if constexpr (internal::is_base_proto<Base>::value) {
            auto dev = static_cast<D*>(this);
            // Can only inherit from one base_protocol implementation.
            ZX_ASSERT(dev->ddk_proto_id_ == 0);
            dev->ddk_proto_id_ = ZX_PROTOCOL_HELLO;
            dev->ddk_proto_ops_ = &hello_protocol_ops_;
        }
    }

    const void* hello_protocol_server_driver() const {
        return hello_protocol_server_driver_;
    }

protected:
    hello_protocol_ops_t hello_protocol_ops_ = {};
    const void* hello_protocol_server_driver_;

private:
    static const void* GetServerDriver(void* ctx) {
        return static_cast<D*>(ctx)->hello_protocol_server_driver();
    }

    static void HelloSay(void* ctx, const char* req, char* out_response, size_t response_capacity) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->HelloSay(req, out_response, response_capacity);
        fdf_env_register_driver_exit();
    }
};

class HelloProtocolClient {
public:
    HelloProtocolClient()
        : ops_(nullptr), ctx_(nullptr) {}
    HelloProtocolClient(const hello_protocol_t* proto)
        : ops_(proto->ops), ctx_(proto->ctx) {}

    HelloProtocolClient(zx_device_t* parent) {
        hello_protocol_t proto;
        if (device_get_protocol(parent, ZX_PROTOCOL_HELLO, &proto) == ZX_OK) {
            ops_ = proto.ops;
            ctx_ = proto.ctx;
        } else {
            ops_ = nullptr;
            ctx_ = nullptr;
        }
    }

    HelloProtocolClient(zx_device_t* parent, const char* fragment_name) {
        hello_protocol_t proto;
        if (device_get_fragment_protocol(parent, fragment_name, ZX_PROTOCOL_HELLO, &proto) == ZX_OK) {
            ops_ = proto.ops;
            ctx_ = proto.ctx;
        } else {
            ops_ = nullptr;
            ctx_ = nullptr;
        }
    }

    // Create a HelloProtocolClient from the given parent device + "fragment".
    //
    // If ZX_OK is returned, the created object will be initialized in |result|.
    static zx_status_t CreateFromDevice(zx_device_t* parent,
                                        HelloProtocolClient* result) {
        hello_protocol_t proto;
        zx_status_t status = device_get_protocol(
                parent, ZX_PROTOCOL_HELLO, &proto);
        if (status != ZX_OK) {
            return status;
        }
        *result = HelloProtocolClient(&proto);
        return ZX_OK;
    }

    // Create a HelloProtocolClient from the given parent device.
    //
    // If ZX_OK is returned, the created object will be initialized in |result|.
    static zx_status_t CreateFromDevice(zx_device_t* parent, const char* fragment_name,
                                        HelloProtocolClient* result) {
        hello_protocol_t proto;
        zx_status_t status = device_get_fragment_protocol(parent, fragment_name,
                                 ZX_PROTOCOL_HELLO, &proto);
        if (status != ZX_OK) {
            return status;
        }
        *result = HelloProtocolClient(&proto);
        return ZX_OK;
    }

    void GetProto(hello_protocol_t* proto) const {
        proto->ctx = ctx_;
        proto->ops = ops_;
    }
    bool is_valid() const {
        return ops_ != nullptr;
    }
    void clear() {
        ctx_ = nullptr;
        ops_ = nullptr;
    }

    void Say(const char* req, char* out_response, size_t response_capacity) const {
        ops_->say(ctx_, req, out_response, response_capacity);
    }

private:
    const hello_protocol_ops_t* ops_;
    void* ctx_;
};

} // namespace ddk
