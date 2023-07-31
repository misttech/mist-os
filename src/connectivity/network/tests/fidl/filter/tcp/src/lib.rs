// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_net_filter as fnetfilter;
use netstack_testing_common::realms::Netstack;
use netstack_testing_macros::netstack_test;

#[netstack_test]
async fn drop_tcp_incoming<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                        /* src_subnet */
            false,                                       /* src_subnet_invert_match */
            None,                                        /* dst_subnet */
            false,                                       /* dst_subnet_invert_match */
            0..=0,                                       /* src_port_range */
            0..=0,                                       /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly, /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                        /* src_subnet */
            false,                                       /* src_subnet_invert_match */
            None,                                        /* dst_subnet */
            false,                                       /* dst_subnet_invert_match */
            0..=0,                                       /* src_port_range */
            0..=0,                                       /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly, /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_incoming_within_port_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                              /* src_subnet */
            false,                                             /* src_subnet_invert_match */
            None,                                              /* dst_subnet */
            false,                                             /* dst_subnet_invert_match */
            common::SERVER_PORT - 1..=common::SERVER_PORT + 1, /* src_port_range */
            0..=0,                                             /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly,       /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing_within_port_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                              /* src_subnet */
            false,                                             /* src_subnet_invert_match */
            None,                                              /* dst_subnet */
            false,                                             /* dst_subnet_invert_match */
            common::SERVER_PORT - 1..=common::SERVER_PORT + 1, /* src_port_range */
            0..=0,                                             /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly,       /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_incoming_outside_port_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                              /* src_subnet */
            false,                                             /* src_subnet_invert_match */
            None,                                              /* dst_subnet */
            false,                                             /* dst_subnet_invert_match */
            common::SERVER_PORT + 1..=common::SERVER_PORT + 3, /* src_port_range */
            common::CLIENT_PORT..=common::CLIENT_PORT,         /* dst_port_range */
            common::ExpectedTraffic::TwoWay,                   /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing_outside_port_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            None,                                              /* src_subnet */
            false,                                             /* src_subnet_invert_match */
            None,                                              /* dst_subnet */
            false,                                             /* dst_subnet_invert_match */
            common::SERVER_PORT + 1..=common::SERVER_PORT + 3, /* src_port_range */
            common::CLIENT_PORT..=common::CLIENT_PORT,         /* dst_port_range */
            common::ExpectedTraffic::TwoWay,                   /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_incoming_with_address_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            false,                            /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            false,                            /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly, /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing_with_address_range<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            false,                            /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            false,                            /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::ClientToServerOnly, /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_incoming_with_src_address_invert<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            true,                             /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            false,                            /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::TwoWay,  /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing_with_src_address_invert<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            true,                             /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            false,                            /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::TwoWay,  /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_incoming_with_dst_address_invert<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::client_incoming_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            false,                            /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            true,                             /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::TwoWay,  /* expected_traffic */
        ),
    )
    .await
}

#[netstack_test]
async fn drop_tcp_outgoing_with_dst_address_invert<N: Netstack>(name: &str) {
    common::test_filter::<N>(
        name,
        common::server_outgoing_drop_test(
            fnetfilter::SocketProtocol::Tcp,
            Some(common::SERVER_IPV4_SUBNET), /* src_subnet */
            false,                            /* src_subnet_invert_match */
            Some(common::CLIENT_IPV4_SUBNET), /* dst_subnet */
            true,                             /* dst_subnet_invert_match */
            0..=0,                            /* src_port_range */
            0..=0,                            /* dst_port_range */
            common::ExpectedTraffic::TwoWay,  /* expected_traffic */
        ),
    )
    .await
}
