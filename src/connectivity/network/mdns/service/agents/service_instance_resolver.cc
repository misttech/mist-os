// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/network/mdns/service/agents/service_instance_resolver.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include "src/connectivity/network/mdns/service/common/mdns_fidl_util.h"
#include "src/connectivity/network/mdns/service/common/mdns_names.h"
#include "src/connectivity/network/mdns/service/common/type_converters.h"

namespace mdns {
namespace {
constexpr zx::duration kAdditionalInterval = zx::sec(1);
constexpr uint32_t kAdditionalIntervalMultiplier = 2;
constexpr uint32_t kAdditionalMaxQueries = 3;
}  // namespace

ServiceInstanceResolver::ServiceInstanceResolver(MdnsAgent::Owner* owner,
                                                 const std::string& service,
                                                 const std::string& instance, zx::time timeout,
                                                 Media media, IpVersions ip_versions,
                                                 bool include_local, bool include_local_proxies,
                                                 Mdns::ResolveServiceInstanceCallback callback)
    : MdnsAgent(owner),
      service_(service),
      instance_name_(instance),
      timeout_(timeout),
      media_(media),
      ip_versions_(ip_versions),
      include_local_(include_local),
      include_local_proxies_(include_local_proxies),
      callback_(std::move(callback)) {
  FX_DCHECK(callback_);
}

ServiceInstanceResolver::~ServiceInstanceResolver() {}

void ServiceInstanceResolver::Quit() {
  if (callback_) {
    callback_(std::move(instance_));
    callback_ = nullptr;
  }

  MdnsAgent::Quit();
}

void ServiceInstanceResolver::EndOfMessage() {
  if (!callback_) {
    // This can happen when a redundant response is received after the block below runs and before
    // the posted task runs, e.g. when two NICs are connected to the same LAN.
    return;
  }

  // if srv was received but has no aaaa record, send aaaa query.
  // Since nsec is not supported, the following check is intentional as
  // ServiceInstanceResolver supports only v6 addresses.
  if (instance_.has_service() && !instance_.has_ipv6_endpoint()) {
    Query(DnsType::kAaaa, target_full_name_, media_, ip_versions_, now(), kAdditionalInterval,
          kAdditionalIntervalMultiplier, kAdditionalMaxQueries);
    return;
  }

  if (port_.is_valid() && instance_.has_ipv6_endpoint()) {
    callback_(std::move(instance_));
    callback_ = nullptr;
    PostTaskForTime([this]() { RemoveSelf(); }, now());
  }
}

void ServiceInstanceResolver::Start(const std::string& service_instance) {
  MdnsAgent::Start(service_instance);
  service_instance_ = MdnsNames::InstanceFullName(instance_name_, service_);
  // Increase the chance of coalescing the queries together in one message.
  auto ts = now();
  Query(DnsType::kSrv, service_instance_, media_, ip_versions_, ts, kAdditionalInterval,
        kAdditionalIntervalMultiplier, kAdditionalMaxQueries);
  Query(DnsType::kTxt, service_instance_, media_, ip_versions_, ts, kAdditionalInterval,
        kAdditionalIntervalMultiplier, kAdditionalMaxQueries);

  PostTaskForTime(
      [this]() {
        if (callback_) {
          callback_(std::move(instance_));
          callback_ = nullptr;
          RemoveSelf();
        }
      },
      timeout_);
}

void ServiceInstanceResolver::ReceiveResource(const DnsResource& resource,
                                              MdnsResourceSection section,
                                              ReplyAddress sender_address) {
  if (!sender_address.Matches(media_) || !sender_address.Matches(ip_versions_)) {
    return;
  }

  switch (resource.type_) {
    case DnsType::kSrv:
      if (resource.name_.dotted_string_ == service_instance_) {
        instance_.set_service(service_);
        instance_.set_instance(instance_name_);
        instance_.set_srv_priority(resource.srv_.priority_);
        instance_.set_srv_weight(resource.srv_.weight_);
        port_ = resource.srv_.port_;
        target_full_name_ = resource.srv_.target_.dotted_string_;
        instance_.set_target(MdnsNames::HostNameFromFullName(target_full_name_));
      }
      break;
    case DnsType::kA:
      if (resource.name_.dotted_string_ == target_full_name_) {
        auto address = MdnsFidlUtil::CreateSocketAddressV4(
            inet::SocketAddress(resource.a_.address_.address_, port_));
        instance_.set_ipv4_endpoint(address);
        if (!instance_.has_addresses()) {
          instance_.set_addresses(std::vector<fuchsia::net::SocketAddress>());
        }

        instance_.mutable_addresses()->push_back(
            fuchsia::net::SocketAddress::WithIpv4(std::move(address)));
      }
      break;
    case DnsType::kAaaa:
      if (resource.name_.dotted_string_ == target_full_name_) {
        // Add scope_id only to link local addresses.
        uint32_t scope_id = 0;
        if (resource.aaaa_.address_.address_.is_link_local()) {
          scope_id = sender_address.interface_id();
        }
        auto address = MdnsFidlUtil::CreateSocketAddressV6(
            inet::SocketAddress(resource.aaaa_.address_.address_, port_, scope_id));
        instance_.set_ipv6_endpoint(address);
        if (!instance_.has_addresses()) {
          instance_.set_addresses(std::vector<fuchsia::net::SocketAddress>());
        }

        instance_.mutable_addresses()->push_back(
            fuchsia::net::SocketAddress::WithIpv6(std::move(address)));
      }
      break;
    case DnsType::kTxt:
      if (resource.name_.dotted_string_ == target_full_name_) {
        instance_.set_text(fidl::To<std::vector<std::string>>(resource.txt_.strings_));
        instance_.set_text_strings(fidl::Clone(resource.txt_.strings_));
      }
      break;
    default:
      break;
  }
}

void ServiceInstanceResolver::OnAddLocalServiceInstance(const Mdns::ServiceInstance& instance,
                                                        bool from_proxy) {
  if (!callback_) {
    return;
  }

  if (from_proxy ? !include_local_proxies_ : !include_local_) {
    return;
  }

  if (instance.service_name_ != service_ || instance.instance_name_ != instance_name_) {
    return;
  }

  callback_(fidl::To<fuchsia::net::mdns::ServiceInstance>(instance));
  callback_ = nullptr;
  PostTaskForTime([this]() { RemoveSelf(); }, now());
}

}  // namespace mdns
