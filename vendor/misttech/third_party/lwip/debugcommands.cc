// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/*
 * Copyright (c) 2013 Corey Tabaka
 *
 * Permission is hereby granted, free of charge, to any person obtaining
 * a copy of this software and associated documentation files
 * (the "Software"), to deal in the Software without restriction,
 * including without limitation the rights to use, copy, modify, merge,
 * publish, distribute, sublicense, and/or sell copies of the Software,
 * and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 * IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
 * CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 * TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

#include <lib/console.h>
#include <stdio.h>
#include <string.h>

#include <lwip/api.h>
#include <lwip/dhcp.h>
#include <lwip/ip_addr.h>
#include <lwip/netif.h>

namespace {

void usage(const cmd_args* argv) {
  printf("%s commands:\n", argv[0].str);
  printf("%s dns lookup <hostname>\n", argv[0].str);
  printf("%s dhcp start <nicid>\n", argv[0].str);
  printf("%s dhcp stop <nicid>\n", argv[0].str);
  printf("%s if list\n", argv[0].str);
  printf("%s if enable <nicid>\n", argv[0].str);
  printf("%s if disable <nicid>\n", argv[0].str);
  printf("%s route list\n", argv[0].str);
  printf("%s route add <destination> <gateway> <nicid>\n", argv[0].str);
  printf("%s route delete <destination>\n", argv[0].str);
}

void do_dns_lookup(const cmd_args* argv) {
  if (!strcmp(argv[2].str, "lookup")) {
    ip_addr_t ip_addr;
    err_t err = netconn_gethostbyname(argv[3].str, &ip_addr);
    if (err != ERR_OK) {
      printf("DNS lookup failed: %d\n", err);
    } else {
      printf("%s: %u.%u.%u.%u\n", argv[3].str, ip4_addr1_16(&ip_addr), ip4_addr2_16(&ip_addr),
             ip4_addr3_16(&ip_addr), ip4_addr4_16(&ip_addr));
    }
  } else {
    usage(argv);
  }
}

void do_dhcp(const cmd_args* argv) {
  if (!strcmp(argv[2].str, "start")) {
    auto nicid = atoi(argv[3].str);
    struct netif* netif = netif_get_by_index(static_cast<u8_t>(nicid));
    if (netif == nullptr) {
      printf("Interface %s not found\n", argv[3].str);
      return;
    }

    err_t err = dhcp_start(netif);
    if (err == ERR_OK) {
      printf("dhcp client started on interface %d\n", nicid);
    } else {
      printf("failed to start dhcp client on interface %d: %d\n", nicid, err);
    }
  } else if (!strcmp(argv[2].str, "stop")) {
    auto nicid = atoi(argv[3].str);
    struct netif* netif = netif_get_by_index(static_cast<u8_t>(nicid));
    if (netif == nullptr) {
      printf("Interface %s not found\n", argv[3].str);
      return;
    }
    dhcp_stop(netif);
    printf("dhcp client stopped on interface %d\n", nicid);
  } else {
    usage(argv);
  }
}

void do_if(const cmd_args* argv) {
  if (!strcmp(argv[2].str, "list")) {
    printf("Interface List:\n");
    for (u8_t i = 1; i <= 254; ++i) {
      struct netif* netif = netif_get_by_index(i);
      if (netif == nullptr) {
        continue;
      }

      printf("nicid             %d\n", i);
      printf("name              %c%c%c\n", netif->name[0], netif->name[1], '0' + netif->num);
      printf("device class      %s\n",
             (netif->flags & NETIF_FLAG_ETHARP) ? "ethernet" : "loopback");
      printf("online            %s\n", netif_is_up(netif) ? "true" : "false");

      const char* default_routes = "-";
      if ((netif->flags & NETIF_FLAG_ETHARP) && (netif->flags & NETIF_FLAG_MLD6)) {
        default_routes = "IPv4,IPv6";
      } else if (netif->flags & NETIF_FLAG_ETHARP) {
        default_routes = "IPv4";
      } else if (netif->flags & NETIF_FLAG_MLD6) {
        default_routes = "IPv6";
      }
      printf("default routes    %s\n", default_routes);

      if (!ip4_addr_isany_val(netif->ip_addr)) {
        char ip_str[16];
        ip4addr_ntoa_r(&netif->ip_addr, ip_str, sizeof(ip_str));
        char netmask_str[16];
        ip4addr_ntoa_r(&netif->netmask, netmask_str, sizeof(netmask_str));
        printf("addr              %s/%s\n", ip_str, netmask_str);
      }

      if (netif->hwaddr_len == 6) {
        printf("mac               %02X:%02X:%02X:%02X:%02X:%02X\n\n", netif->hwaddr[0],
               netif->hwaddr[1], netif->hwaddr[2], netif->hwaddr[3], netif->hwaddr[4],
               netif->hwaddr[5]);
      } else {
        printf("mac               -\n\n");
      }
    }
  } else if (!strcmp(argv[2].str, "enable")) {
    struct netif* netif = netif_get_by_index(static_cast<u8_t>(atoi(argv[3].str)));
    if (netif == nullptr) {
      printf("Interface %s not found\n", argv[3].str);
      return;
    }
    netif_set_up(netif);
    printf("Interface %s enabled\n", argv[3].str);
  } else if (!strcmp(argv[2].str, "disable")) {
    struct netif* netif = netif_get_by_index(static_cast<u8_t>(atoi(argv[3].str)));
    if (netif == nullptr) {
      printf("Interface %s not found\n", argv[3].str);
      return;
    }
    netif_set_down(netif);
    printf("Interface %s disabled\n", argv[3].str);
  } else {
    usage(argv);
  }
}

void do_route(const cmd_args* argv) {
  if (!strcmp(argv[2].str, "list")) {
    printf("%-18s %-18s %-6s %-6s %-6s\n", "Destination", "Gateway", "NICID", "Metric", "TableId");
    printf("------------------------------------------------------------\n");
    for (struct netif* netif = netif_list; netif != nullptr; netif = netif->next) {
      char ip_str[16], gw_str[16];
      ip4addr_ntoa_r(&netif->ip_addr, ip_str, sizeof(ip_str));
      ip4addr_ntoa_r(&netif->gw, gw_str, sizeof(gw_str));
      printf("%-18s %-18s %-6d %-6d %-6d\n", ip_str, gw_str, netif_get_index(netif), 0, 0);
    }
  } else {
    usage(argv);
  }
}

int net_cmd(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 3) {
    usage(argv);
    return 0;
  }

  if (!strcmp(argv[1].str, "if")) {
    do_if(argv);
  } else if (!strcmp(argv[1].str, "dns")) {
    do_dns_lookup(argv);
  } else if (!strcmp(argv[1].str, "route")) {
    do_route(argv);
  } else if (!strcmp(argv[1].str, "dhcp")) {
    do_dhcp(argv);
  } else {
    usage(argv);
  }

  return 0;
}

}  // namespace

STATIC_COMMAND_START
STATIC_COMMAND("net", "net toolbox", &net_cmd)
STATIC_COMMAND_END(net)
