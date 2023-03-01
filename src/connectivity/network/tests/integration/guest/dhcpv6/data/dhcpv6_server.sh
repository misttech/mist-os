#!/usr/bin/env bash
#
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# https://vaneyckt.io/posts/safer_bash_scripts_with_set_euxo_pipefail/
set -Eeuxo pipefail

#
# Script for managing running one or more DHCPv6 servers on the guest
# consisting of three subcommands:
#
# - `setup` for performing one-time initial setup,
# - `start` for starting a DHCPv6 server (can be called multiple times), and
# - `stop` for stopping a DHCPv6 server.
#
# See documentation of each subcommand below.

usage() {
  echo "usage: ${0} setup|start|stop" >&2
  exit 1
}

stop_usage() {
  echo "usage: ${0} stop <id>" >&2
  exit 1
}

#######################################
# Find an interface that isn't loopback.
# Arguments:
#   None
# Outputs:
#   The name of an interface other than loopback.
#######################################
find_iface() {
  for iface_path in /sys/class/net/*; do
    candidate=$(basename "${iface_path}")
    if [[ "${candidate}" != "lo" ]]; then
      echo "${candidate}"
      return
    fi
  done

  echo "Unable to find an non-loopback interface" >&2
  exit 1
}

if [[ $# -eq 0 ]]; then
  usage
fi

readonly subcommand="${1}"
shift

case "${subcommand}" in
  #
  # The `setup` subcommand performs one-time basic setup steps.
  #
  # Does not take any arguments. Must be called before the first usage of
  # `start` to start a DHCPv6 server.
  #
  setup)
    # Stop NetworkManager to prevent it from attempting to manage the
    # interface we want to run a DHCPv6 server on (it may start a
    # DHCPv6 client and cause problems).
    systemctl mask NetworkManager.service
    systemctl stop NetworkManager.service

    # Stop the instance of DHCPv6 server that's run through a systemd unit
    # to prevent interference.
    systemctl stop isc-dhcp-server

    iface=$(find_iface)
    ip link set dev "${iface}" up
    ;;
  #
  # The `start` subcommand starts an instance of isc-dhcp-server's DHCPv6 server.
  #
  # Optional Arguments:
  #
  #   --prefix-low    The lowest value of the server's prefix pool in the form
  #                   of an IPv6 address without prefix length.
  #   --prefix-high   The highest value of the server's prefix pool in the form
  #                   of an IPv6 address without prefix length.
  #   --prefix-len    The length of prefixes the server assigns as an integer
  #                   in the range [1, 128] inclusive.
  #
  # Positional Arguments:
  #   id            An identifier for disambiguating between different servers
  #                 started from this script. Also determines the contents of
  #                 the Server ID option.
  #   subnet        This value will be used in the subnet6 declaration in the
  #                 DHCPv6 server configuration, and must be passed solely to
  #                 appease isc-dhcp-server.
  #   server-addr   Must be an address in `subnet`, and will be assigned to the
  #                 interface the DHCPv6 server will be started on.
  #
  # Example:
  #
  # ```
  # dhcpv6_server.sh start \
  #   --prefix-low 2001:db8:1:: \
  #   --prefix-high 2001:db8:2:: \
  #   --prefix-len 64 \
  #   server1 2001:db8::/16 2001:db8::1/16
  # ```
  #
  start)
    start_usage() {
      echo "usage: ${0} start [--prefix-low <prefix> --prefix-high <prefix> --prefix-len <len>] <id> <subnet> <server-addr>" >&2
      exit 1
    }

    PARSED_ARGS=$(getopt --options "" --long prefix-low:,prefix-high:,prefix-len: -- "$@")
    eval set -- "$PARSED_ARGS"

    while :
    do
      case "${1}" in
        --prefix-low)
          readonly prefix_low="${2}"
          shift 2
          ;;
        --prefix-high)
          readonly prefix_high="${2}"
          shift 2
          ;;
        --prefix-len)
          readonly prefix_len="${2}"
          shift 2
          ;;
        --)
          shift

          if [[ $# -ne 3 ]]; then
            echo "start subcommand must be invoked with three positional arguments" >&2
            start_usage
          fi
          readonly id="${1}"
          readonly subnet="${2}"
          readonly server_addr="${3}"
          break
          ;;
      esac
    done

    readonly tmpdir=/tmp/"${id}"
    mkdir "${tmpdir}"
    readonly conf="${tmpdir}"/dhcpd.conf
    readonly lease_file="${tmpdir}"/dhcpd.leases
    readonly pid_file="${tmpdir}"/dhcpd.pid

    iface_name=$(find_iface)

    # isc-dhcp-server requires an IPv6 address belonging to the subnet to be
    # assigned to the interface only for the purpose of symmetry with respect
    # to IPv4 (DHCPv6 doesn't have this requirement as a link-local IPv6
    # suffices).
    #
    # Note that the presence of an address inside the subnet named by the
    # subnet6 section in the config file is how isc-dhcp-server knows which
    # interface to run the DHCPv6 server on.
    ip addr add dev "${iface_name}" "${server_addr}"

    # The contents of the Server ID option needs to be explicitly specified
    # so that multiple servers don't accidentally end up with the same
    # identifier.
    echo "option dhcp6.server-id \"${id}\";" >>"${conf}"
    echo "max-lease-time 10;" >>"${conf}"
    echo "default-lease-time 10;" >>"${conf}"

    if [[ -n "${subnet}" ]]; then
      echo "subnet6 ${subnet} {" >>"${conf}"

      if [[ -n "${prefix_low}" && -n "${prefix_len}" && -n "${prefix_high}" ]]; then
        echo "prefix6 ${prefix_low} ${prefix_high} / ${prefix_len};" >>"${conf}"
      fi

      echo "}" >>"${conf}"
    fi

    # The lease file must exist otherwise isc-dhcp-server refuses to start.
    touch "${lease_file}"

    dhcpd -6 -cf "${conf}" -lf "${lease_file}" -pf "${pid_file}"
    ;;
  #
  # The `stop` subcommand stops a server previously started with `start`.
  #
  # Positional Arguments:
  #
  #   id    The identifier of the server to stop.
  #
  # Example:
  #
  # `dhcpv6_server.sh stop server1`
  #
  stop)
    if [[ $# -ne 1 ]]; then
      stop_usage
    fi
    dir=/tmp/"${1}"
    kill "$(cat "${dir}"/dhcpd.pid)"
    ;;
  *)
    usage
    ;;
esac
