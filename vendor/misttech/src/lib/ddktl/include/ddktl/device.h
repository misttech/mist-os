// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_LIB_DDKTL_INCLUDE_DDKTL_DEVICE_H_
#define VENDOR_MISTTECH_SRC_LIB_DDKTL_INCLUDE_DDKTL_DEVICE_H_

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

// #include <deque>
#include <string>
#include <type_traits>

#include <ddktl/composite-node-spec.h>
#include <ddktl/device-internal.h>
#include <ddktl/init-txn.h>
#include <ddktl/metadata.h>
#include <ddktl/resume-txn.h>
#include <ddktl/suspend-txn.h>
#include <ddktl/unbind-txn.h>

// ddk::Device<D, ...>
//
// Notes:
//
// ddk::Device<D, ...> is a mixin class that simplifies writing DDK drivers in
// C++. The DDK's zx_device_t defines a set of function pointer callbacks that
// can be implemented to define standard behavior (e.g., message),
// as well as to implement device lifecycle events (e.g., unbind/release). The
// mixin classes are used to set up the function pointer table to call methods
// from the user's class automatically.
//
// Every ddk::Device subclass must implement the following release callback to
// cleanup resources:
//
// void DdkRelease();
//
//
// :: Available mixins ::
// +----------------------------+----------------------------------------------------+
// | Mixin class                | Required function implementation                   |
// +----------------------------+----------------------------------------------------+
// | ddk::GetProtocolable       | zx_status_t DdkGetProtocol(uint32_t proto_id,      |
// |                            |                            void* out)              |
// |                            |                                                    |
// | ddk::Initializable         | void DdkInit(ddk::InitTxn txn)                     |
// |                            |                                                    |
// | ddk::Unbindable            | void DdkUnbind(ddk::UnbindTxn txn)                 |
// |                            |                                                    |
// | ddk::PerformanceTunable    | zx_status_t DdkSetPerformanceState(                |
// |                            |                           uint32_t requested_state,|
// |                            |                           uint32_t* out_state)     |
// |                            |                                                    |
// | ddk::AutoSuspendable       | zx_status_t DdkConfigureAutoSuspend(bool enable,   |
// |                            |                      uint8_t requested_sleep_state)|
// |                            |                                                    |
// | ddk::Messageable<P>::Mixin | Methods defined by fidl::WireServer<P>             |
// |                            |                                                    |
// | ddk::Suspendable           | void DdkSuspend(ddk::SuspendTxn txn)               |
// |                            |                                                    |
// | ddk::Resumable             | zx_status_t DdkResume(uint8_t requested_state,     |
// |                            |                          uint8_t* out_state)       |
// |                            |                                                    |
// | ddk::Rxrpcable             | zx_status_t DdkRxrpc(zx_handle_t channel)          |
// |                            |                                                    |
// | ddk::MadeVisible           | zx_status_t DdkMadeVisible()                       |
// +----------------------------+----------------------------------------------------+
//
// :: Example ::
//
// // Define our device type using a type alias.
// class MyDevice;
// using DeviceType = ddk::Device<MyDevice, ddk::Unbindable, ddk::Suspendable>;
//
// class MyDevice : public DeviceType {
//   public:
//     MyDevice(zx_device_t* parent)
//       : DeviceType(parent) {}
//
//     zx_status_t Bind() {
//         // Any other setup required by MyDevice. The device_add_args_t will be filled out by the
//         // base class.
//         return DdkAdd("my-device-name");
//     }
//
//     // Methods required by the ddk mixins
//     void DdkUnbind(ddk::UnbindTxn txn);
//     void DdkSuspend(ddk::SuspendTxn txn);
//     void DdkRelease();
// };
//
// extern "C" zx_status_t my_bind(zx_device_t* device,
//                                void** cookie) {
//     auto dev = make_unique<MyDevice>(device);
//     auto status = dev->Bind();
//     if (status == ZX_OK) {
//         // devmgr is now in charge of the memory for dev
//         dev.release();
//     }
//     return status;
// }
//
// See also: protocol mixins for setting protocol_id and protocol_ops.

namespace ddk {

struct AnyProtocol {
  const void* ops;
  void* ctx;
};

// base_mixin is a tag that all mixins must inherit from.
using base_mixin = internal::base_mixin;

// base_protocol is a tag used by protocol implementations
using base_protocol = internal::base_protocol;

// DDK Device mixins

template <typename D>
class GetProtocolable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckGetProtocolable<D>();
    proto->get_protocol = GetProtocol;
  }

 private:
  static zx_status_t GetProtocol(void* ctx, uint32_t proto_id, void* out) {
    return static_cast<D*>(ctx)->DdkGetProtocol(proto_id, out);
  }
};

template <typename D>
class Initializable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckInitializable<D>();
    proto->init = Init;
  }

 private:
  static void Init(void* ctx) {
    auto dev = static_cast<D*>(ctx);
    InitTxn txn(dev->zxdev());
    dev->DdkInit(std::move(txn));
  }
};

template <typename D>
class Unbindable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckUnbindable<D>();
    proto->unbind = Unbind;
  }

 private:
  static void Unbind(void* ctx) {
    auto dev = static_cast<D*>(ctx);
    UnbindTxn txn(dev->zxdev());
    dev->DdkUnbind(std::move(txn));
  }
};

#if 0
template <typename D, typename Protocol>
class MessageableInternal : public fidl::WireServer<Protocol>, public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) { proto->message = Message; }

 private:
  static void Message(void* ctx, fidl_incoming_msg_t msg, device_fidl_txn_t txn) {
    fidl::WireDispatch<Protocol>(static_cast<D*>(ctx),
                                 fidl::IncomingHeaderAndMessage::FromEncodedCMessage(msg),
                                 ddk::FromDeviceFIDLTransaction(txn));
  }
};

template <typename Protocol>
struct Messageable {
  // This is necessary for currying as this mixin requires two type parameters, which are passed
  // at different times.

  template <typename D>
  using Mixin = MessageableInternal<D, Protocol>;
};
#endif

template <typename D>
class Suspendable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckSuspendable<D>();
    proto->suspend = Suspend_New;
  }

 private:
  static void Suspend_New(void* ctx, uint8_t requested_state, bool enable_wake,
                          uint8_t suspend_reason) {
    auto dev = static_cast<D*>(ctx);
    SuspendTxn txn(dev->zxdev(), requested_state, enable_wake, suspend_reason);
    static_cast<D*>(ctx)->DdkSuspend(std::move(txn));
  }
};

template <typename D>
class AutoSuspendable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckConfigureAutoSuspend<D>();
    proto->configure_auto_suspend = Configure_Auto_Suspend;
  }

 private:
  static zx_status_t Configure_Auto_Suspend(void* ctx, bool enable, uint8_t requested_sleep_state) {
    return static_cast<D*>(ctx)->DdkConfigureAutoSuspend(enable, requested_sleep_state);
  }
};

template <typename D>
class Resumable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckResumable<D>();
    proto->resume = Resume_New;
  }

 private:
  static void Resume_New(void* ctx, uint32_t requested_state) {
    auto dev = static_cast<D*>(ctx);
    ResumeTxn txn(dev->zxdev(), requested_state);
    static_cast<D*>(ctx)->DdkResume(std::move(txn));
  }
};

template <typename D>
class Rxrpcable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckRxrpcable<D>();
    proto->rxrpc = Rxrpc;
  }

 private:
  static zx_status_t Rxrpc(void* ctx, zx_handle_t channel) {
    return static_cast<D*>(ctx)->DdkRxrpc(channel);
  }
};

template <typename D>
class ChildPreReleaseable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckChildPreReleaseable<D>();
    proto->child_pre_release = ChildPreRelease;
  }

 private:
  static void ChildPreRelease(void* ctx, void* child_ctx) {
    static_cast<D*>(ctx)->DdkChildPreRelease(child_ctx);
  }
};

template <typename D>
class MadeVisibleable : public base_mixin {
 protected:
  static constexpr void InitOp(zx_protocol_device_t* proto) {
    internal::CheckMadeVisibleable<D>();
    proto->made_visible = MadeVisible;
  }

 private:
  static void MadeVisible(void* ctx) { static_cast<D*>(ctx)->DdkMadeVisible(); }
};

class MetadataList {
 public:
  MetadataList() = default;
  MetadataList& operator=(const MetadataList& other) {
    // data_list_.clear();
    // metadata_list_.clear();
    ZX_ASSERT(other.metadata_list_.size() == other.data_list_.size());
    for (size_t i = 0; i < other.metadata_list_.size(); ++i) {
      fbl::AllocChecker ac;
      data_list_.push_back(other.data_list_[i], &ac);
      ZX_ASSERT(ac.check());
      metadata_list_.push_back(
          {other.metadata_list_[i].type, data_list_[data_list_.size() - 1]->data(),
           other.metadata_list_[i].length},
          &ac);
      ZX_ASSERT(ac.check());
    }
    return *this;
  }
  MetadataList(const MetadataList& other) { *this = other; }
  zx_status_t AddMetadata(zx_device_t* dev, uint32_t type) {
    auto metadata_blob = GetMetadataBlob(dev, type);
    if (!metadata_blob.is_ok()) {
      return metadata_blob.error_value();
    }
    fbl::AllocChecker ac;

    fbl::Vector<uint8_t> vector;
    for (const auto& byte : *metadata_blob) {
      vector.push_back(byte, &ac);
    }
    ZX_ASSERT(ac.check());

    data_list_.push_back(std::make_shared<fbl::Vector<uint8_t>>(std::move(vector)), &ac);
    ZX_ASSERT(ac.check());

    metadata_list_.push_back(
        {type, data_list_[data_list_.size() - 1]->data(), metadata_blob->size()}, &ac);
    ZX_ASSERT(ac.check());
    return ZX_OK;
  }

  device_metadata_t* data() { return metadata_list_.data(); }
  size_t count() { return metadata_list_.size(); }

 private:
  fbl::Vector<std::shared_ptr<fbl::Vector<uint8_t>>> data_list_;
  fbl::Vector<device_metadata_t> metadata_list_;
};

// Factory functions to create a zx_device_str_prop_t.
inline zx_device_str_prop_t MakeStrProperty(const std::string_view& key, uint32_t val) {
  return {key.data(), str_prop_int_val(val)};
}

inline zx_device_str_prop_t MakeStrProperty(const char* key, uint32_t val) {
  return {key, str_prop_int_val(val)};
}

inline zx_device_str_prop_t MakeStrProperty(const std::string_view& key, bool val) {
  return {key.data(), str_prop_bool_val(val)};
}

inline zx_device_str_prop_t MakeStrProperty(const char* key, bool val) {
  return {key, str_prop_bool_val(val)};
}

inline zx_device_str_prop_t MakeStrProperty(const std::string_view& key,
                                            const std::string_view& val) {
  return {key.data(), str_prop_str_val(val.data())};
}

inline zx_device_str_prop_t MakeStrProperty(const char* key, const std::string_view& val) {
  return {key, str_prop_str_val(val.data())};
}

inline zx_device_str_prop_t MakeStrProperty(const std::string_view& key, const char* val) {
  return {key.data(), str_prop_str_val(val)};
}

inline zx_device_str_prop_t MakeStrProperty(const char* key, const char* val) {
  return {key, str_prop_str_val(val)};
}

class DeviceAddArgs {
 public:
  explicit DeviceAddArgs(const char* name) {
    args_.name = name;
    args_.version = DEVICE_ADD_ARGS_VERSION;
  }

  DeviceAddArgs& operator=(const DeviceAddArgs& other) {
    metadata_list_ = other.metadata_list_;
    args_ = other.args_;
    args_.metadata_list = metadata_list_.data();
    args_.metadata_count = metadata_list_.count();
    return *this;
  }

  DeviceAddArgs(const DeviceAddArgs& other) { *this = other; }

  DeviceAddArgs& set_name(const char* name) {
    args_.name = name;
    return *this;
  }
  DeviceAddArgs& set_flags(uint32_t flags) {
    args_.flags = flags;
    return *this;
  }
  DeviceAddArgs& set_context(void* ctx) {
    args_.ctx = ctx;
    return *this;
  }
  DeviceAddArgs& set_str_props(cpp20::span<const zx_device_str_prop_t> props) {
    args_.str_props = props.data();
    args_.str_prop_count = static_cast<uint32_t>(props.size());
    return *this;
  }
  DeviceAddArgs& set_proto_id(uint32_t proto_id) {
    args_.proto_id = proto_id;
    return *this;
  }
  DeviceAddArgs& set_ops(zx_protocol_device_t* ops) {
    args_.ops = ops;
    return *this;
  }
#ifndef __mist_os__
  DeviceAddArgs& set_inspect_vmo(zx::vmo inspect_vmo) {
    args_.inspect_vmo = inspect_vmo.release();
    return *this;
  }
#endif
  DeviceAddArgs& forward_metadata(zx_device_t* dev, uint32_t type) {
    if (ZX_OK == metadata_list_.AddMetadata(dev, type)) {
      args_.metadata_list = metadata_list_.data();
      args_.metadata_count = metadata_list_.count();
    }
    return *this;
  }

#ifndef __mist_os__
  DeviceAddArgs& set_outgoing_dir(zx::channel outgoing_dir) {
    args_.outgoing_dir_channel = outgoing_dir.release();
    return *this;
  }

  DeviceAddArgs& set_fidl_service_offers(cpp20::span<const char*> fidl_service_offers) {
    args_.fidl_service_offers = fidl_service_offers.data();
    args_.fidl_service_offer_count = fidl_service_offers.size();
    return *this;
  }
  DeviceAddArgs& set_runtime_service_offers(cpp20::span<const char*> runtime_service_offers) {
    args_.runtime_service_offers = runtime_service_offers.data();
    args_.runtime_service_offer_count = runtime_service_offers.size();
    return *this;
  }
#endif
  DeviceAddArgs& set_power_states(cpp20::span<const device_power_state_info_t> power_states) {
    args_.power_states = power_states.data();
    args_.power_state_count = static_cast<uint8_t>(power_states.size());
    return *this;
  }
#ifndef __mist_os__
  DeviceAddArgs& set_bus_info(std::unique_ptr<fuchsia_driver_framework::BusInfo> bus_info) {
    bus_info_ = std::move(bus_info);
    args_.bus_info = bus_info_.get();
    return *this;
  }
#endif

  const device_add_args_t& get() const { return args_; }

 private:
  MetadataList metadata_list_;
#ifndef __mist_os__
  std::unique_ptr<fuchsia_driver_framework::BusInfo> bus_info_;
#endif
  device_add_args_t args_ = {};
};

// Device is templated on the list of mixins that define which DDK device
// methods are implemented. Note that internal::base_device *must* be the
// left-most base class in order to ensure that its constructor runs before the
// mixin constructors. This ensures that ddk_device_proto_ is zero-initialized
// before setting the fields in the mixins.
template <class D, template <typename> class... Mixins>
class Device : public ::ddk::internal::base_device<D, Mixins...> {
 public:
  zx_status_t DdkAdd(const char* name, device_add_args_t args) {
    if (this->zxdev_ != nullptr) {
      return ZX_ERR_BAD_STATE;
    }

    args.version = DEVICE_ADD_ARGS_VERSION;
    args.name = name;
    // Since we are stashing this as a D*, we can use ctx in all
    // the callback functions and cast it directly to a D*.
    args.ctx = static_cast<D*>(this);
    args.ops = &this->ddk_device_proto_;
    AddProtocol(&args);

    if (name) {
      this->name_ = name;
    }

    return device_add(this->parent_, &args, &this->zxdev_);
  }

  zx_status_t DdkAdd(const DeviceAddArgs& args) { return DdkAdd(args.get().name, args.get()); }

  zx_status_t DdkAdd(const char* name, uint32_t flags = 0) {
    return DdkAdd(ddk::DeviceAddArgs(name).set_flags(flags));
  }

  zx_status_t DdkAddCompositeNodeSpec(const char* name, const CompositeNodeSpec& spec) {
    return device_add_composite_spec(this->parent_, name, &spec.get());
  }

  // Schedules the removal of the device and its descendents.
  // Each device will evenutally have its unbind hook (if implemented) and release hook invoked.
  void DdkAsyncRemove() {
    ZX_ASSERT(this->zxdev_ != nullptr);

    zx_device_t* dev = this->zxdev_;
    device_async_remove(dev);
  }

#ifndef __mist_os__
  // AddService allows a driver to advertise a FIDL service.
  // The intended use is for drivers to use this to advertise services to non-drivers.
  // It is only really supported in the compat shim, where it adds the service to the outgoing
  // directory that the compat shim maintains.
  // The service is added to the outgoing directory of the parent device, which
  // for compat drivers will always exist.
  // |handler| is the handler for the service.
  template <typename Service, typename = std::enable_if_t<fidl::IsServiceV<Service>>>
  zx::result<> DdkAddService(
      fidl::ServiceInstanceHandler<fidl::internal::ChannelTransport> handler) {
    constexpr char kInstanceName[] = "default";
    auto handlers = handler.TakeMemberHandlers();
    if (handlers.empty()) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    std::string service_name = Service::Name;

    for (auto& [member_name, member_handler] : handlers) {
      zx_status_t status =
          device_register_service_member(this->parent_, static_cast<void*>(&member_handler),
                                         service_name.c_str(), kInstanceName, member_name.c_str());
      if (status != ZX_OK) {
        return zx::make_result(status);
      }
    }
    return zx::ok();
  }

  zx_status_t DdkGetMetadataSize(uint32_t type, size_t* out_size) {
    // Uses parent() instead of zxdev() as metadata is usually checked
    // before DdkAdd(). There are few use cases to actually call it on self.
    return device_get_metadata_size(parent(), type, out_size);
  }

  zx_status_t DdkGetMetadata(uint32_t type, void* buf, size_t buf_len, size_t* actual) {
    // Uses parent() instead of zxdev() as metadata is usually checked
    // before DdkAdd(). There are few use cases to actually call it on self.
    return device_get_metadata(parent(), type, buf, buf_len, actual);
  }

  zx_status_t DdkAddMetadata(uint32_t type, const void* data, size_t length) {
    return device_add_metadata(zxdev(), type, data, length);
  }

  zx_status_t DdkGetFragmentMetadata(const char* name, uint32_t type, void* buf, size_t buf_len,
                                     size_t* actual) {
    // Uses parent() instead of zxdev() as metadata is usually checked
    // before DdkAdd(). There are few use cases to actually call it on self.
    return device_get_fragment_metadata(parent(), name, type, buf, buf_len, actual);
  }

  zx_status_t DdkGetFragmentProtocol(const char* name, uint32_t proto_id, void* out) {
    return device_get_fragment_protocol(zxdev(), name, proto_id, out);
  }

  template <typename Protocol, typename = std::enable_if_t<fidl::IsProtocolV<Protocol>>>
  zx::result<fidl::ClientEnd<Protocol>> DdkConnectNsProtocol() const {
    return DdkConnectNsProtocol<Protocol>(parent());
  }

  template <typename Protocol, typename = std::enable_if_t<fidl::IsProtocolV<Protocol>>>
  static zx::result<fidl::ClientEnd<Protocol>> DdkConnectNsProtocol(zx_device_t* parent) {
    auto endpoints = fidl::CreateEndpoints<Protocol>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto status = device_connect_ns_protocol(parent, fidl::DiscoverableProtocolName<Protocol>,
                                             endpoints->server.TakeChannel().release());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }

  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>> DdkConnectFidlProtocol() const {
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>);

    return DdkConnectFidlProtocol<ServiceMember>(parent());
  }

  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  static zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>> DdkConnectFidlProtocol(
      zx_device_t* parent) {
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>);

    auto endpoints = fidl::CreateEndpoints<typename ServiceMember::ProtocolType>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto status =
        device_connect_fidl_protocol2(parent, ServiceMember::ServiceName, ServiceMember::Name,
                                      endpoints->server.TakeChannel().release());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }

  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>> DdkConnectFragmentFidlProtocol(
      const char* fragment_name) const {
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>);

    return DdkConnectFragmentFidlProtocol<ServiceMember>(parent(), fragment_name);
  }

  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  static zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>>
  DdkConnectFragmentFidlProtocol(zx_device_t* parent, const char* fragment_name) {
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>);

    auto endpoints = fidl::CreateEndpoints<typename ServiceMember::ProtocolType>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto status = device_connect_fragment_fidl_protocol(
        parent, fragment_name, ServiceMember::ServiceName, ServiceMember::Name,
        endpoints->server.TakeChannel().release());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }

  template <typename ServiceMember>
  zx::result<fdf::ClientEnd<typename ServiceMember::ProtocolType>> DdkConnectRuntimeProtocol()
      const {
    static_assert(fidl::IsServiceMemberV<ServiceMember>);
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::DriverTransport>);

    return DdkConnectRuntimeProtocol<ServiceMember>(parent());
  }

  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fdf::ClientEnd<typename ServiceMember::ProtocolType>>
  DdkConnectFragmentRuntimeProtocol(const char* fragment_name) const {
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::DriverTransport>);

    return DdkConnectFragmentRuntimeProtocol<ServiceMember>(parent(), fragment_name);
  }

  template <typename ServiceMember>
  static zx::result<fdf::ClientEnd<typename ServiceMember::ProtocolType>> DdkConnectRuntimeProtocol(
      zx_device_t* parent) {
    static_assert(fidl::IsServiceMemberV<ServiceMember>);
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::DriverTransport>);

    auto endpoints = fdf::CreateEndpoints<typename ServiceMember::ProtocolType>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto status =
        device_connect_runtime_protocol(parent, ServiceMember::ServiceName, ServiceMember::Name,
                                        endpoints->server.TakeChannel().release());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }

  template <typename ServiceMember>
  static zx::result<fdf::ClientEnd<typename ServiceMember::ProtocolType>>
  DdkConnectFragmentRuntimeProtocol(zx_device_t* parent, const char* fragment_name) {
    static_assert(fidl::IsServiceMemberV<ServiceMember>);
    static_assert(std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::DriverTransport>);

    auto endpoints = fdf::CreateEndpoints<typename ServiceMember::ProtocolType>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto status = device_connect_fragment_runtime_protocol(
        parent, fragment_name, ServiceMember::ServiceName, ServiceMember::Name,
        endpoints->server.TakeChannel().release());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }
#endif

  const char* name() const { return this->name_.data(); }

  // The opaque pointer representing this device.
  zx_device_t* zxdev() const { return this->zxdev_; }
  // The opaque pointer representing the device's parent.
  zx_device_t* parent() const { return this->parent_; }

 protected:
  explicit Device(zx_device_t* parent) : internal::base_device<D, Mixins...>(parent) {
    internal::CheckMixins<Mixins<D>...>();
    internal::CheckReleasable<D>();
  }

 private:
  // Add the protocol id and ops if D inherits from a base_protocol implementation.
  template <typename T = D>
  void AddProtocol(
      device_add_args_t* args,
      typename std::enable_if<internal::is_base_protocol<T>::value, T>::type* dummy = 0) {
    auto dev = static_cast<D*>(this);
    ZX_ASSERT(dev->ddk_proto_id_ > 0);
    args->proto_id = dev->ddk_proto_id_;
    args->proto_ops = dev->ddk_proto_ops_;
  }

  // If D does not inherit from a base_protocol implementation, do nothing.
  template <typename T = D>
  void AddProtocol(
      device_add_args_t* args,
      typename std::enable_if<!internal::is_base_protocol<T>::value, T>::type* dummy = 0) {}
};

}  // namespace ddk

#endif  // VENDOR_MISTTECH_SRC_LIB_DDKTL_INCLUDE_DDKTL_DEVICE_H_
