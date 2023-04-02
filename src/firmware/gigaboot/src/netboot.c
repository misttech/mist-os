// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "netboot.h"

#include <device_id.h>
#include <inet6.h>
#include <lib/netboot/netboot.h>
#include <netifc.h>
#include <stdio.h>
#include <string.h>
#include <xefi.h>

#include <tftp/tftp.h>

#define TFTP_BUF_SZ 2048
char tftp_session_scratch[TFTP_BUF_SZ];
char tftp_out_scratch[TFTP_BUF_SZ];

// item being downloaded
static nbfile_t* item;

// TFTP file state
typedef struct {
  nbfile_t* netboot_file_data;
  size_t file_size;
  unsigned int progress_reported;
} file_info_t;

// TFTP transport state
typedef struct {
  struct ip6_addr_t dest_addr;
  uint16_t dest_port;
} transport_info_t;

static uint32_t last_cookie = 0;
static uint32_t last_cmd = 0;
static uint32_t last_arg = 0;
static uint32_t last_ack_cmd = 0;
static uint32_t last_ack_arg = 0;

static int nb_boot_now = 0;
static int nb_active = 0;

static char advertise_nodename[64] = "";
static char advertise_data[256] = "nodename=zircon";

static void send_query_ack(const ip6_addr* addr, uint16_t port, uint32_t cookie) {
  uint8_t buffer[256];
  netboot_message_header_t* hdr = (void*)buffer;
  uint8_t* payload = (uint8_t*)(hdr + 1);
  hdr->magic = NETBOOT_MAGIC;
  hdr->cookie = cookie;
  hdr->cmd = NETBOOT_COMMAND_ACK;
  hdr->arg = NETBOOT_VERSION_CURRENT;
  memcpy(payload, advertise_nodename, sizeof(advertise_nodename));
  udp6_send(buffer, sizeof(netboot_message_header_t) + strlen(advertise_nodename) + 1, addr, port,
            NETBOOT_PORT_SERVER);
}

static void advertise(void) {
  uint8_t buffer[sizeof(netboot_message_header_t) + sizeof(advertise_data)];
  netboot_message_header_t* hdr = (void*)buffer;
  uint8_t* payload = (uint8_t*)(hdr + 1);
  hdr->magic = NETBOOT_MAGIC;
  hdr->cookie = 0;
  hdr->cmd = NETBOOT_COMMAND_ADVERTISE;
  hdr->arg = NETBOOT_VERSION_CURRENT;
  size_t data_len = strlen(advertise_data) + 1;
  memcpy(payload, advertise_data, data_len);
  udp6_send(buffer, sizeof(netboot_message_header_t) + data_len, &ip6_ll_all_nodes,
            NETBOOT_PORT_ADVERT, NETBOOT_PORT_SERVER);
}

void netboot_recv(void* data, size_t len, const ip6_addr* saddr, uint16_t sport) {
  netboot_message_header_t* hdr = data;
  uint8_t* payload = (uint8_t*)(hdr + 1);
  netboot_message_header_t ack;
  int do_transmit = 1;

  if (len < sizeof(netboot_message_header_t))
    return;
  len -= sizeof(netboot_message_header_t);

  // printf("netboot: MSG %08x %08x %08x %08x datalen %zu\n",
  //        hdr->magic, hdr->cookie, hdr->cmd, hdr->arg, len);

  if ((last_cookie == hdr->cookie) && (last_cmd == hdr->cmd) && (last_arg == hdr->arg)) {
    // host must have missed the ack. resend
    ack.magic = NETBOOT_MAGIC;
    ack.cookie = last_cookie;
    ack.cmd = last_ack_cmd;
    ack.arg = last_ack_arg;
    goto transmit;
  }

  ack.cmd = NETBOOT_COMMAND_ACK;
  ack.arg = 0;

  switch (hdr->cmd) {
    case NETBOOT_COMMAND_EXECUTE:
      if (len == 0)
        return;
      payload[len - 1] = 0;
      break;
    case NETBOOT_COMMAND_SEND_FILE:
      if (len == 0)
        return;
      payload[len - 1] = 0;
      for (size_t i = 0; i < (len - 1); i++) {
        if ((payload[i] < ' ') || (payload[i] > 127)) {
          payload[i] = '.';
        }
      }
      item = netboot_get_buffer((const char*)payload, hdr->arg);
      if (item) {
        item->offset = 0;
        ack.arg = hdr->arg;
        size_t prefix_len = strlen(NETBOOT_FILENAME_PREFIX);
        const char* filename;
        if (!strncmp((char*)payload, NETBOOT_FILENAME_PREFIX, prefix_len)) {
          filename = &((const char*)payload)[prefix_len];
        } else {
          filename = (const char*)payload;
        }
        printf("netboot: Receive File '%s'...\n", filename);
      } else {
        printf("netboot: Rejected File '%s'...\n", (char*)payload);
        ack.cmd = NETBOOT_COMMAND_ERROR_BAD_FILE;
      }
      break;

    case NETBOOT_COMMAND_DATA:
    case NETBOOT_COMMAND_LAST_DATA:
      if (item == 0) {
        printf("netboot: > received chunk before NETBOOT_FILE\n");
        return;
      }
      if (hdr->arg != item->offset) {
        // printf("netboot: < received chunk at offset %d but current offset is %zu\n", hdr->arg,
        // item->offset);
        ack.arg = item->offset;
        ack.cmd = NETBOOT_COMMAND_ACK;
      } else if ((item->offset + len) > item->size) {
        ack.cmd = NETBOOT_COMMAND_ERROR_TOO_LARGE;
        ack.arg = hdr->arg;
      } else {
        memcpy(item->data + item->offset, payload, len);
        item->offset += len;
        ack.cmd = hdr->cmd == NETBOOT_COMMAND_LAST_DATA ? NETBOOT_COMMAND_FILE_RECEIVED
                                                        : NETBOOT_COMMAND_ACK;
        if (hdr->cmd != NETBOOT_COMMAND_LAST_DATA) {
          do_transmit = 0;
        }
      }
      break;
    case NETBOOT_COMMAND_BOOT:
      nb_boot_now = 1;
      printf("netboot: Boot Kernel...\n");
      break;
    case NETBOOT_COMMAND_QUERY:
      // Send reply and return w/o getting the netboot state out of sync.
      send_query_ack(saddr, sport, hdr->cookie);
      return;
    default:
      ack.cmd = NETBOOT_COMMAND_ERROR_BAD_CMD;
      ack.arg = 0;
  }

  last_cookie = hdr->cookie;
  last_cmd = hdr->cmd;
  last_arg = hdr->arg;
  last_ack_cmd = ack.cmd;
  last_ack_arg = ack.arg;

  ack.cookie = hdr->cookie;
  ack.magic = NETBOOT_MAGIC;
transmit:
  nb_active = 1;
  if (do_transmit) {
    // printf("netboot: MSG %08x %08x %08x %08x\n",
    //   ack.magic, ack.cookie, ack.cmd, ack.arg);

    udp6_send(&ack, sizeof(ack), saddr, sport, NETBOOT_PORT_SERVER);
  }
}

static tftp_status buffer_open(const char* filename, size_t size, uint8_t session_timeout_secs,
                               void* cookie) {
  file_info_t* file_info = cookie;
  file_info->netboot_file_data = netboot_get_buffer(filename, size);
  if (file_info->netboot_file_data == NULL) {
    printf("netboot: unrecognized file %s - rejecting\n", filename);
    return TFTP_ERR_INVALID_ARGS;
  }
  file_info->netboot_file_data->offset = 0;
  const char* base_filename;
  size_t prefix_len = strlen(NETBOOT_FILENAME_PREFIX);
  if (!strncmp(filename, NETBOOT_FILENAME_PREFIX, prefix_len)) {
    base_filename = &filename[prefix_len];
  } else {
    base_filename = filename;
  }
  printf("Receiving %s [%lu bytes]... ", base_filename, (unsigned long)size);
  file_info->file_size = size;
  file_info->progress_reported = 0;
  return TFTP_NO_ERROR;
}

static tftp_status buffer_write(const void* data, size_t* len, off_t offset, void* cookie) {
  file_info_t* file_info = cookie;
  nbfile_t* nb_buf_info = file_info->netboot_file_data;
  if (offset > nb_buf_info->size || (offset + *len) > nb_buf_info->size) {
    printf("netboot: attempt to write past end of buffer\n");
    return TFTP_ERR_INVALID_ARGS;
  }
  memcpy(&nb_buf_info->data[offset], data, *len);
  nb_buf_info->offset = offset + *len;
  if (file_info->file_size >= 100) {
    unsigned int progress_pct = offset / (file_info->file_size / 100);
    if ((progress_pct > file_info->progress_reported) &&
        (progress_pct - file_info->progress_reported >= 5)) {
      printf("%u%%... ", progress_pct);
      file_info->progress_reported = progress_pct;
    }
  }
  return TFTP_NO_ERROR;
}

static void buffer_close(void* cookie) {
  file_info_t* file_info = cookie;
  file_info->netboot_file_data = NULL;
  printf("Done\n");
}

static tftp_status udp_send(void* data, size_t len, void* cookie) {
  transport_info_t* transport_info = cookie;
  int bytes_sent = udp6_send(data, len, &transport_info->dest_addr, transport_info->dest_port,
                             NETBOOT_PORT_TFTP_OUTGOING);
  return bytes_sent < 0 ? TFTP_ERR_IO : TFTP_NO_ERROR;
}

static int udp_timeout_set(uint32_t timeout_ms, void* cookie) {
  // TODO
  return 0;
}

static int strcmp8to16(const char* str8, const char16_t* str16) {
  while (*str8 != '\0' && *str8 == *str16) {
    str8++;
    str16++;
  }
  return *str8 - *str16;
}

void tftp_recv(void* data, size_t len, const ip6_addr* daddr, uint16_t dport, const ip6_addr* saddr,
               uint16_t sport) {
  static tftp_session* session = NULL;
  static file_info_t file_info = {.netboot_file_data = NULL};
  static transport_info_t transport_info = {};

  if (dport == NETBOOT_PORT_TFTP_INCOMING) {
    if (session != NULL) {
      printf("Aborting to service new connection\n");
    }
    // Start TFTP session
    int ret = tftp_init(&session, tftp_session_scratch, sizeof(tftp_session_scratch));
    if (ret != TFTP_NO_ERROR) {
      printf("netboot: failed to initiate tftp session\n");
      session = NULL;
      return;
    }

    // Override our window size on the Acer tablet
    if (!strcmp8to16("INSYDE Corp.", gSys->FirmwareVendor)) {
      uint16_t window_size = 8;
      tftp_set_options(session, NULL, NULL, &window_size);
    }

    // Initialize file interface
    tftp_file_interface file_ifc = {NULL, buffer_open, NULL, buffer_write, buffer_close};
    tftp_session_set_file_interface(session, &file_ifc);

    // Initialize transport interface
    memcpy(&transport_info.dest_addr, saddr, sizeof(struct ip6_addr_t));
    transport_info.dest_port = sport;
    tftp_transport_interface transport_ifc = {udp_send, NULL, udp_timeout_set};
    tftp_session_set_transport_interface(session, &transport_ifc);
  } else if (!session) {
    // Ignore anything sent to the outgoing port unless we've already established a connection
    return;
  }

  size_t outlen = sizeof(tftp_out_scratch);

  char err_msg[128];
  tftp_handler_opts handler_opts = {.inbuf = data,
                                    .inbuf_sz = len,
                                    .outbuf = tftp_out_scratch,
                                    .outbuf_sz = &outlen,
                                    .err_msg = err_msg,
                                    .err_msg_sz = sizeof(err_msg)};
  tftp_status status = tftp_handle_msg(session, &transport_info, &file_info, &handler_opts);
  if (status < 0) {
    printf("netboot: tftp protocol error: %s\n", err_msg);
    session = NULL;
  } else if (status == TFTP_TRANSFER_COMPLETED) {
    session = NULL;
  }
}

#define FAST_TICK 100
#define SLOW_TICK 1000

int netboot_init(const char* nodename, uint32_t namegen) {
  if (netifc_open()) {
    printf("netboot: Failed to open network interface\n");
    return -1;
  }
  char buf[DEVICE_ID_MAX];
  if (!nodename || (nodename[0] == 0)) {
    device_id(eth_addr(), buf, namegen);
    nodename = buf;
  }
  if (nodename) {
    strncpy(advertise_nodename, nodename, sizeof(advertise_nodename) - 1);
    snprintf(advertise_data, sizeof(advertise_data), "version=%s;nodename=%s",
             NETBOOT_BOOTLOADER_VERSION, nodename);
  }
  return 0;
}

const char* netboot_nodename(void) { return advertise_nodename; }

static int nb_fastcount = 0;
static int nb_online = 0;

int netboot_poll(void) {
  if (netifc_active()) {
    if (nb_online == 0) {
      printf("netboot: interface online\n");
      nb_online = 1;
      nb_fastcount = 20;
      netifc_set_timer(FAST_TICK);
      advertise();
    }
  } else {
    if (nb_online == 1) {
      printf("netboot: interface offline\n");
      nb_online = 0;
    }
    return 0;
  }
  if (netifc_timer_expired()) {
    if (nb_fastcount) {
      nb_fastcount--;
      netifc_set_timer(FAST_TICK);
    } else {
      netifc_set_timer(SLOW_TICK);
    }
    if (nb_active) {
      // don't advertise if we're in a transfer
      nb_active = 0;
    } else {
      advertise();
    }
  }

  netifc_poll();

  if (nb_boot_now) {
    nb_boot_now = 0;
    return 1;
  } else {
    return 0;
  }
}

void netboot_close(void) { netifc_close(); }
