// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <trace.h>

#include <lwip/mistos/inferfaces_admin.h>
#include <lwip/netif.h>
#include <lwip/tcpip.h>
#include <netif/etharp.h>

#include "vendor/misttech/src/connectivity/lib/network-device/cpp/network_device_client.h"

#define LOCAL_TRACE 0

namespace lwip::mistos {

namespace {

err_t low_level_output(struct netif* netif, struct pbuf* p) {
  LTRACEF("netif %p buf %p\n", netif, p);

  State* state = static_cast<State*>(netif->state);
  if (!state) {
    return ERR_ABRT;
  }
  struct pbuf* q;

#if ETH_PAD_SIZE
  pbuf_remove_header(p, ETH_PAD_SIZE); /* drop the padding word */
#endif

  for (q = p; q != nullptr; q = q->next) {
    network::client::NetworkDeviceClient::Buffer buffer = state->client->AllocTx();
    if (!buffer.is_valid()) {
      return ERR_ABRT;
    }
    buffer.data().SetFrameType(FRAME_TYPE_ETHERNET);
    buffer.data().SetPortId(state->port_id);
    LTRACEF("q->len: %d\n", q->len);
    buffer.data().Write(q->payload, q->len);
    if (buffer.Send() != ZX_OK) {
      return ERR_ABRT;
    }
  }

  // MIB2_STATS_NETIF_ADD(netif, ifoutoctets, p->tot_len);
  if (((u8_t*)p->payload)[0] & 1) {
    /* broadcast or multicast packet*/
    // MIB2_STATS_NETIF_INC(netif, ifoutnucastpkts);
  } else {
    /* unicast packet */
    // MIB2_STATS_NETIF_INC(netif, ifoutucastpkts);
  }

#if ETH_PAD_SIZE
  pbuf_add_header(p, ETH_PAD_SIZE); /* reclaim the padding word */
#endif

  LINK_STATS_INC(link.xmit);

  return ERR_OK;
}

static struct pbuf* low_level_input(struct netif* netif,
                                    network::client::NetworkDeviceClient::Buffer& buffer) {
  ZX_ASSERT(netif != nullptr);

  State* state = static_cast<State*>(netif->state);
  if (!state) {
    return nullptr;
  }
  struct pbuf *p, *q;
  u16_t len = static_cast<u16_t>(buffer.data().len());

  /* We allocate a pbuf chain of pbufs from the pool. */
  p = pbuf_alloc(PBUF_RAW, len, PBUF_POOL);

  if (p != nullptr) {
#if ETH_PAD_SIZE
    pbuf_remove_header(p, ETH_PAD_SIZE); /* drop the padding word */
#endif

    /* We iterate over the pbuf chain until we have read the entire
     * packet into the pbuf. */
    for (q = p; q != nullptr; q = q->next) {
      /* Read enough bytes to fill this pbuf in the chain. The
       * available data in the pbuf is given by the q->len
       * variable.
       * This does not necessarily have to be a memcpy, you can also preallocate
       * pbufs for a DMA-enabled MAC and after receiving truncate it to the
       * actually received size. In this case, ensure the tot_len member of the
       * pbuf is the sum of the chained pbuf len members.
       */
      buffer.data().Read(q->payload, q->len);
    }

    // MIB2_STATS_NETIF_ADD(netif, ifinoctets, p->tot_len);
    if (((u8_t*)p->payload)[0] & 1) {
      /* broadcast or multicast packet*/
      // MIB2_STATS_NETIF_INC(netif, ifinnucastpkts);
    } else {
      /* unicast packet*/
      // MIB2_STATS_NETIF_INC(netif, ifinucastpkts);
    }
#if ETH_PAD_SIZE
    pbuf_add_header(p, ETH_PAD_SIZE); /* reclaim the padding word */
#endif

    LINK_STATS_INC(link.recv);
  } else {
    // drop packet();
    LINK_STATS_INC(link.memerr);
    LINK_STATS_INC(link.drop);
    // MIB2_STATS_NETIF_INC(netif, ifindiscards);
  }
  return p;
}

}  // namespace

err_t CreateInterface(struct netif* netif) {
  netif->name[0] = 'e';
  netif->name[1] = 'n';
  netif->hostname = nullptr;

  State* state = static_cast<State*>(netif->state);
  if (!state) {
    return ERR_ARG;
  }

  auto client = state->client;
  if (!client) {
    return ERR_ARG;
  }

  netif->output = etharp_output;
  netif->linkoutput = low_level_output;
  netif->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP;
  //  netif->output_ip6 = ethip6_output;

  zx_status_t get_port_info_status = ZX_OK;
  client->GetPortInfoWithMac(
      state->port_id,
      [&get_port_info_status, &netif](zx::result<network::client::PortInfoAndMac> result) {
        if (result.is_error()) {
          get_port_info_status = result.error_value();
          return;
        }
        auto& port_info = result.value();
        std::ranges::copy(port_info.unicast_address->octets, std::begin(netif->hwaddr));
        netif->hwaddr_len = sizeof(port_info.unicast_address->octets);
      });

  if (get_port_info_status != ZX_OK) {
    return ERR_ARG;
  }

  {
    auto watcher = client->WatchStatus(state->port_id, [&netif](zx::result<port_status_t> result) {
      if (result.is_error()) {
        return;
      }
      auto& status = result.value();
      netif->mtu = static_cast<u16_t>(status.mtu);
      if (status.flags & STATUS_FLAGS_ONLINE) {
        netif->flags |= NETIF_FLAG_LINK_UP;
      } else {
        netif->flags &= ~NETIF_FLAG_LINK_UP;
      }
    });

    if (watcher.is_error()) {
      return ERR_ARG;
    }
  }

  zx_status_t open_session_status = ZX_OK;
  client->OpenSession("lwip", [&](zx_status_t status) {
    if (status != ZX_OK) {
      LTRACEF("Failed to open session: %s\n", zx_status_get_string(status));
      open_session_status = status;
      return;
    }
  });

  if (open_session_status != ZX_OK) {
    return ERR_ABRT;
  }

  fbl::Vector<frame_type_t> frame_types;
  fbl::AllocChecker ac;
  frame_types.push_back(FRAME_TYPE_ETHERNET, &ac);
  if (!ac.check()) {
    return ERR_MEM;
  }

  zx_status_t attach_port_status = ZX_OK;
  client->AttachPort(state->port_id, std::move(frame_types), [&](zx_status_t status) {
    if (status != ZX_OK) {
      LTRACEF("failed to attach port %d: %s\n", state->port_id.base, zx_status_get_string(status));
      attach_port_status = status;
      return;
    }
    LTRACEF("attached port %d\n", state->port_id.base);
  });

  if (attach_port_status != ZX_OK) {
    return ERR_ABRT;
  }

  client->SetRxCallback([netif](network::client::NetworkDeviceClient::Buffer buffer) {
    LTRACEF("rx callback %d\n", buffer.data().len());
    ZX_ASSERT_MSG(buffer.data().frame_type() == FRAME_TYPE_ETHERNET,
                  "expected ethernet frame type");
    struct pbuf* p;

    /* move received packet into a new pbuf */
    p = low_level_input(netif, buffer);
    /* if no packet could be read, silently ignore this */
    if (p != nullptr) {
      /* pass all packets to ethernet_input, which decides what packets it supports */
      if (netif->input(p, netif) != ERR_OK) {
        LTRACEF("rx callback: IP input error\n");
        pbuf_free(p);
        p = nullptr;
      }
    }
  });

  return ERR_OK;
}

}  // namespace lwip::mistos
