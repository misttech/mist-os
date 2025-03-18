// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_THIRD_PARTY_LWIP_LWIPOPTS_H_
#define VENDOR_MISTTECH_THIRD_PARTY_LWIP_LWIPOPTS_H_

#include <errno.h>

#include <arch/defines.h>

// clang-format off

/* An OS is present */
#define NO_SYS    0

// use zircon's libc malloc
// use mem_malloc() which calls malloc()
// instead of creating static memory pools
#define MEM_LIBC_MALLOC   1
#define MEMP_MEM_MALLOC   1
#define MEM_USE_POOLS     0

/* Sockets API config */
#define LWIP_COMPAT_SOCKETS       0
#define LWIP_SOCKET_OFFSET        1
#define LWIP_POLL                 1

#define LWIP_DHCP 1
#define LWIP_AUTOIP 1
#define LWIP_DHCP_AUTOIP_COOP 1

#define LWIP_DNS 1
#define LWIP_IGMP 1
#define LWIP_RAW 1

#define LWIP_NETIF_HOSTNAME 1
#define LWIP_NETIF_API 1
#define LWIP_NETIF_STATUS_CALLBACK 1
#define LWIP_NETIF_HWADDRHINT 1

/*
 * Activate loopback, but don't use lwip's default loopback interface,
 * we provide our own.
 */
#define LWIP_NETIF_LOOPBACK   1
#define LWIP_HAVE_LOOPIF      1

/* Disable stats */
#define LWIP_STATS          0
#define LWIP_STATS_DISPLAY  0

// #define LWIP_TIMEVAL_PRIVATE 0
#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 1

/* Debug mode */
#ifdef LWIP_DEBUG
#define ETHARP_DEBUG      LWIP_DBG_OFF
#define NETIF_DEBUG       LWIP_DBG_OFF
#define PBUF_DEBUG        LWIP_DBG_OFF
#define API_LIB_DEBUG     LWIP_DBG_OFF
#define API_MSG_DEBUG     LWIP_DBG_OFF
#define SOCKETS_DEBUG     LWIP_DBG_OFF
#define ICMP_DEBUG        LWIP_DBG_OFF
#define IGMP_DEBUG        LWIP_DBG_OFF
#define INET_DEBUG        LWIP_DBG_OFF
#define IP_DEBUG          LWIP_DBG_OFF
#define IP_REASS_DEBUG    LWIP_DBG_OFF
#define RAW_DEBUG         LWIP_DBG_OFF
#define MEM_DEBUG         LWIP_DBG_OFF
#define MEMP_DEBUG        LWIP_DBG_OFF
#define SYS_DEBUG         LWIP_DBG_OFF
#define TIMERS_DEBUG      LWIP_DBG_OFF
#define TCP_DEBUG         LWIP_DBG_OFF
#define TCP_INPUT_DEBUG   LWIP_DBG_OFF
#define TCP_FR_DEBUG      LWIP_DBG_OFF
#define TCP_RTO_DEBUG     LWIP_DBG_OFF
#define TCP_CWND_DEBUG    LWIP_DBG_OFF
#define TCP_WND_DEBUG     LWIP_DBG_OFF
#define TCP_OUTPUT_DEBUG  LWIP_DBG_OFF
#define TCP_RST_DEBUG     LWIP_DBG_OFF
#define TCP_QLEN_DEBUG    LWIP_DBG_OFF
#define UDP_DEBUG         LWIP_DBG_OFF
#define TCPIP_DEBUG       LWIP_DBG_OFF
#define SLIP_DEBUG        LWIP_DBG_OFF
#define DHCP_DEBUG        LWIP_DBG_OFF
#define AUTOIP_DEBUG      LWIP_DBG_OFF
#define DNS_DEBUG         LWIP_DBG_OFF
#define IP6_DEBUG         LWIP_DBG_OFF
#endif

// clang-format on

#endif  // VENDOR_MISTTECH_THIRD_PARTY_LWIP_LWIPOPTS_H_
