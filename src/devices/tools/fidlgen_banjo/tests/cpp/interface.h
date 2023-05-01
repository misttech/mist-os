// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.interface banjo file

#pragma once

#include <banjo/examples/interface/c/banjo.h>
#include <ddktl/device-internal.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/env.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include "banjo-internal.h"

// DDK interface-protocol support
//
// :: Proxies ::
//
// ddk::BakerProtocolClient is a simple wrapper around
// baker_protocol_t. It does not own the pointers passed to it.
//
// :: Mixins ::
//
// ddk::BakerProtocol is a mixin class that simplifies writing DDK drivers
// that implement the baker protocol. It doesn't set the base protocol.
//
// :: Examples ::
//
// // A driver that implements a ZX_PROTOCOL_BAKER device.
// class BakerDevice;
// using BakerDeviceType = ddk::Device<BakerDevice, /* ddk mixins */>;
//
// class BakerDevice : public BakerDeviceType,
//                      public ddk::BakerProtocol<BakerDevice> {
//   public:
//     BakerDevice(zx_device_t* parent)
//         : BakerDeviceType(parent) {}
//
//     void BakerRegister(const cookie_maker_protocol_t* intf, const cookie_jarrer_protocol_t* jar);
//
//     void BakerChange(const change_args_t* payload, change_args_t* out_payload);
//
//     void BakerDeRegister();
//
//     ...
// };

namespace ddk {
// An interface for a device that's able to create and deliver cookies!

template <typename D, bool runtime_enforce_no_reentrancy = false>
class CookieMakerProtocol : public internal::base_mixin {
public:
    CookieMakerProtocol() {
        cookie_maker_protocol_server_driver_ = fdf_env_get_current_driver();
        internal::CheckCookieMakerProtocolSubclass<D>();
        cookie_maker_protocol_ops_.prep = CookieMakerPrep;
        cookie_maker_protocol_ops_.bake = CookieMakerBake;
        cookie_maker_protocol_ops_.deliver = CookieMakerDeliver;
    }

    const void* cookie_maker_protocol_server_driver() const {
        return cookie_maker_protocol_server_driver_;
    }

protected:
    cookie_maker_protocol_ops_t cookie_maker_protocol_ops_ = {};
    const void* cookie_maker_protocol_server_driver_;

private:
    static const void* GetServerDriver(void* ctx) {
        return static_cast<D*>(ctx)->cookie_maker_protocol_server_driver();
    }

    // Asynchonously preps a cookie.
    static void CookieMakerPrep(void* ctx, cookie_kind_t cookie, cookie_maker_prep_callback callback, void* cookie) {
        if (callback && cookie) {
            struct AsyncCallbackWrapper {
                const void* driver;
                cookie_maker_prep_callback callback;
                void* cookie;
            };

            AsyncCallbackWrapper* wrapper = new AsyncCallbackWrapper {
                fdf_env_get_current_driver(),
                callback,
                cookie,
            };

            cookie = wrapper;
            callback = [](void* ctx, uint64_t token) {
                AsyncCallbackWrapper* wrapper = static_cast<AsyncCallbackWrapper*>(ctx);
                fdf_env_register_driver_entry(wrapper->driver, runtime_enforce_no_reentrancy);
                wrapper->callback(wrapper->cookie, token);
                fdf_env_register_driver_exit();
                delete wrapper;
            };
        }
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->CookieMakerPrep(cookie, callback, cookie);
        fdf_env_register_driver_exit();
    }
    // Asynchonously bakes a cookie.
    // Must only be called after preping finishes.
    static void CookieMakerBake(void* ctx, uint64_t token, zx_time_t time, cookie_maker_bake_callback callback, void* cookie) {
        if (callback && cookie) {
            struct AsyncCallbackWrapper {
                const void* driver;
                cookie_maker_bake_callback callback;
                void* cookie;
            };

            AsyncCallbackWrapper* wrapper = new AsyncCallbackWrapper {
                fdf_env_get_current_driver(),
                callback,
                cookie,
            };

            cookie = wrapper;
            callback = [](void* ctx, zx_status_t s) {
                AsyncCallbackWrapper* wrapper = static_cast<AsyncCallbackWrapper*>(ctx);
                fdf_env_register_driver_entry(wrapper->driver, runtime_enforce_no_reentrancy);
                wrapper->callback(wrapper->cookie, s);
                fdf_env_register_driver_exit();
                delete wrapper;
            };
        }
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->CookieMakerBake(token, time, callback, cookie);
        fdf_env_register_driver_exit();
    }
    // Synchronously deliver a cookie.
    // Must be called only after Bake finishes.
    static zx_status_t CookieMakerDeliver(void* ctx, uint64_t token) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        auto ret = static_cast<D*>(ctx)->CookieMakerDeliver(token);
        fdf_env_register_driver_exit();
        return ret;
    }
};

class CookieMakerProtocolClient {
public:
    CookieMakerProtocolClient()
        : ops_(nullptr), ctx_(nullptr) {}
    CookieMakerProtocolClient(const cookie_maker_protocol_t* proto)
        : ops_(proto->ops), ctx_(proto->ctx) {}

    void GetProto(cookie_maker_protocol_t* proto) const {
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

    // Asynchonously preps a cookie.
    void Prep(cookie_kind_t cookie, cookie_maker_prep_callback callback, void* cookie) const {
        ops_->prep(ctx_, cookie, callback, cookie);
    }

    // Asynchonously bakes a cookie.
    // Must only be called after preping finishes.
    void Bake(uint64_t token, zx_time_t time, cookie_maker_bake_callback callback, void* cookie) const {
        ops_->bake(ctx_, token, time, callback, cookie);
    }

    // Synchronously deliver a cookie.
    // Must be called only after Bake finishes.
    zx_status_t Deliver(uint64_t token) const {
        return ops_->deliver(ctx_, token);
    }

private:
    const cookie_maker_protocol_ops_t* ops_;
    void* ctx_;
};
// An interface for storing cookies.

template <typename D, bool runtime_enforce_no_reentrancy = false>
class CookieJarrerProtocol : public internal::base_mixin {
public:
    CookieJarrerProtocol() {
        cookie_jarrer_protocol_server_driver_ = fdf_env_get_current_driver();
        internal::CheckCookieJarrerProtocolSubclass<D>();
        cookie_jarrer_protocol_ops_.place = CookieJarrerPlace;
        cookie_jarrer_protocol_ops_.take = CookieJarrerTake;
    }

    const void* cookie_jarrer_protocol_server_driver() const {
        return cookie_jarrer_protocol_server_driver_;
    }

protected:
    cookie_jarrer_protocol_ops_t cookie_jarrer_protocol_ops_ = {};
    const void* cookie_jarrer_protocol_server_driver_;

private:
    static const void* GetServerDriver(void* ctx) {
        return static_cast<D*>(ctx)->cookie_jarrer_protocol_server_driver();
    }

    // Place a cookie in the named jar. If no jar with the supplied name exists, one is created.
    static void CookieJarrerPlace(void* ctx, const char* name) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->CookieJarrerPlace(name);
        fdf_env_register_driver_exit();
    }
    // Who took a cookie from the cookie jar?
    static cookie_kind_t CookieJarrerTake(void* ctx, const char* name) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        auto ret = static_cast<D*>(ctx)->CookieJarrerTake(name);
        fdf_env_register_driver_exit();
        return ret;
    }
};

class CookieJarrerProtocolClient {
public:
    CookieJarrerProtocolClient()
        : ops_(nullptr), ctx_(nullptr) {}
    CookieJarrerProtocolClient(const cookie_jarrer_protocol_t* proto)
        : ops_(proto->ops), ctx_(proto->ctx) {}

    void GetProto(cookie_jarrer_protocol_t* proto) const {
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

    // Place a cookie in the named jar. If no jar with the supplied name exists, one is created.
    void Place(const char* name) const {
        ops_->place(ctx_, name);
    }

    // Who took a cookie from the cookie jar?
    cookie_kind_t Take(const char* name) const {
        return ops_->take(ctx_, name);
    }

private:
    const cookie_jarrer_protocol_ops_t* ops_;
    void* ctx_;
};
// Protocol for a baker who outsources all of it's baking duties to others.

template <typename D, typename Base = internal::base_mixin, bool runtime_enforce_no_reentrancy = false>
class BakerProtocol : public Base {
public:
    BakerProtocol() {
        baker_protocol_server_driver_ = fdf_env_get_current_driver();
        internal::CheckBakerProtocolSubclass<D>();
        baker_protocol_ops_.register = BakerRegister;
        baker_protocol_ops_.change = BakerChange;
        baker_protocol_ops_.de_register = BakerDeRegister;

        if constexpr (internal::is_base_proto<Base>::value) {
            auto dev = static_cast<D*>(this);
            // Can only inherit from one base_protocol implementation.
            ZX_ASSERT(dev->ddk_proto_id_ == 0);
            dev->ddk_proto_id_ = ZX_PROTOCOL_BAKER;
            dev->ddk_proto_ops_ = &baker_protocol_ops_;
        }
    }

    const void* baker_protocol_server_driver() const {
        return baker_protocol_server_driver_;
    }

protected:
    baker_protocol_ops_t baker_protocol_ops_ = {};
    const void* baker_protocol_server_driver_;

private:
    static const void* GetServerDriver(void* ctx) {
        return static_cast<D*>(ctx)->baker_protocol_server_driver();
    }

    // Registers a cookie maker device which the baker can use, and a cookie jar into
    // which they can place their completed cookies.
    static void BakerRegister(void* ctx, const cookie_maker_protocol_t* intf, const cookie_jarrer_protocol_t* jar) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->BakerRegister(intf, jar);
        fdf_env_register_driver_exit();
    }
    // Swap out the maker or jarrer for a different one.
    static void BakerChange(void* ctx, const change_args_t* payload, change_args_t* out_payload) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->BakerChange(payload, out_payload);
        fdf_env_register_driver_exit();
    }
    // De-registers a cookie maker device when it's no longer available.
    static void BakerDeRegister(void* ctx) {
        fdf_env_register_driver_entry(GetServerDriver(ctx), runtime_enforce_no_reentrancy);
        static_cast<D*>(ctx)->BakerDeRegister();
        fdf_env_register_driver_exit();
    }
};

class BakerProtocolClient {
public:
    BakerProtocolClient()
        : ops_(nullptr), ctx_(nullptr) {}
    BakerProtocolClient(const baker_protocol_t* proto)
        : ops_(proto->ops), ctx_(proto->ctx) {}

    BakerProtocolClient(zx_device_t* parent) {
        baker_protocol_t proto;
        if (device_get_protocol(parent, ZX_PROTOCOL_BAKER, &proto) == ZX_OK) {
            ops_ = proto.ops;
            ctx_ = proto.ctx;
        } else {
            ops_ = nullptr;
            ctx_ = nullptr;
        }
    }

    BakerProtocolClient(zx_device_t* parent, const char* fragment_name) {
        baker_protocol_t proto;
        if (device_get_fragment_protocol(parent, fragment_name, ZX_PROTOCOL_BAKER, &proto) == ZX_OK) {
            ops_ = proto.ops;
            ctx_ = proto.ctx;
        } else {
            ops_ = nullptr;
            ctx_ = nullptr;
        }
    }

    // Create a BakerProtocolClient from the given parent device + "fragment".
    //
    // If ZX_OK is returned, the created object will be initialized in |result|.
    static zx_status_t CreateFromDevice(zx_device_t* parent,
                                        BakerProtocolClient* result) {
        baker_protocol_t proto;
        zx_status_t status = device_get_protocol(
                parent, ZX_PROTOCOL_BAKER, &proto);
        if (status != ZX_OK) {
            return status;
        }
        *result = BakerProtocolClient(&proto);
        return ZX_OK;
    }

    // Create a BakerProtocolClient from the given parent device.
    //
    // If ZX_OK is returned, the created object will be initialized in |result|.
    static zx_status_t CreateFromDevice(zx_device_t* parent, const char* fragment_name,
                                        BakerProtocolClient* result) {
        baker_protocol_t proto;
        zx_status_t status = device_get_fragment_protocol(parent, fragment_name,
                                 ZX_PROTOCOL_BAKER, &proto);
        if (status != ZX_OK) {
            return status;
        }
        *result = BakerProtocolClient(&proto);
        return ZX_OK;
    }

    void GetProto(baker_protocol_t* proto) const {
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

    // Registers a cookie maker device which the baker can use, and a cookie jar into
    // which they can place their completed cookies.
    void Register(void* intf_ctx, const cookie_maker_protocol_ops_t* intf_ops, void* jar_ctx, const cookie_jarrer_protocol_ops_t* jar_ops) const {
        const cookie_maker_protocol_t intf2 = {
            .ops = intf_ops,
            .ctx = intf_ctx,
        };
        const cookie_maker_protocol_t* intf = &intf2;
        const cookie_jarrer_protocol_t jar2 = {
            .ops = jar_ops,
            .ctx = jar_ctx,
        };
        const cookie_jarrer_protocol_t* jar = &jar2;
        ops_->register(ctx_, intf, jar);
    }

    // Swap out the maker or jarrer for a different one.
    void Change(const change_args_t* payload, change_args_t* out_payload) const {
        ops_->change(ctx_, payload, out_payload);
    }

    // De-registers a cookie maker device when it's no longer available.
    void DeRegister() const {
        ops_->de_register(ctx_);
    }

private:
    const baker_protocol_ops_t* ops_;
    void* ctx_;
};

} // namespace ddk
