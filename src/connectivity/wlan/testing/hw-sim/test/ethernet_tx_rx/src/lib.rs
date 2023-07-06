// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_policy as fidl_policy,
    fuchsia_zircon::DurationNum,
    ieee80211::Bssid,
    netdevice_client,
    pin_utils::pin_mut,
    wlan_common::{
        bss::Protection,
        buffer_reader::BufferReader,
        channel::{Cbw, Channel},
        mac,
    },
    wlan_hw_sim::{
        connect_with_security_type, default_wlantap_config_client, event, init_syslog,
        loop_until_iface_is_found, netdevice_helper, rx_wlan_data_frame, test_utils,
        EventHandlerBuilder, TxHandlerBuilder, AP_SSID, CLIENT_MAC_ADDR, ETH_DST_MAC,
    },
};

const BSS: Bssid = Bssid([0x65, 0x74, 0x68, 0x6e, 0x65, 0x74]);
const PAYLOAD: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

async fn send_and_receive<'a>(
    session: &'a netdevice_client::Session,
    port: &'a netdevice_client::Port,
    buf: &'a [u8],
) -> (mac::EthernetIIHdr, Vec<u8>) {
    netdevice_helper::send(session, port, &buf).await;
    let recv_buf = netdevice_helper::recv(session).await;
    let mut buf_reader = BufferReader::new(&recv_buf[..]);
    let header = buf_reader
        .read::<mac::EthernetIIHdr>()
        .expect("bytes received too short for ethernet header");
    let payload = buf_reader.into_remaining().to_vec();
    (*header, payload)
}

async fn verify_tx_and_rx(
    session: &netdevice_client::Session,
    port: &netdevice_client::Port,
    helper: &mut test_utils::TestHelper,
) {
    let mut buf: Vec<u8> = Vec::new();
    netdevice_helper::write_fake_frame(ETH_DST_MAC, CLIENT_MAC_ADDR, PAYLOAD, &mut buf);

    let tx_rx_fut = send_and_receive(session, port, &buf);
    pin_mut!(tx_rx_fut);

    let phy = helper.proxy();
    let mut actual = Vec::new();
    let mut handler = EventHandlerBuilder::new()
        .on_tx(
            TxHandlerBuilder::new()
                .on_data_frame(|data_frame: &Vec<u8>| {
                    for msdu in mac::MsduIterator::from_raw_data_frame(&data_frame[..], false)
                        .expect("reading msdu from data frame")
                    {
                        let mac::Msdu { dst_addr, src_addr, llc_frame } = msdu;
                        if dst_addr == ETH_DST_MAC && src_addr == CLIENT_MAC_ADDR {
                            assert_eq!(llc_frame.hdr.protocol_id.to_native(), mac::ETHER_TYPE_IPV4);
                            actual.clear();
                            actual.extend_from_slice(llc_frame.body);
                            rx_wlan_data_frame(
                                &Channel::new(1, Cbw::Cbw20),
                                &CLIENT_MAC_ADDR,
                                &BSS.0,
                                &ETH_DST_MAC,
                                &PAYLOAD,
                                mac::ETHER_TYPE_IPV4,
                                &phy,
                            )
                            .expect("sending wlan data frame");
                        }
                    }
                })
                .build(),
        )
        .build();
    let (header, payload) = helper
        .run_until_complete_or_timeout(
            5.seconds(),
            "verify ethernet_tx_rx",
            event::matched(move |_, event| handler(event)),
            tx_rx_fut,
        )
        .await;
    assert_eq!(&actual[..], PAYLOAD);
    assert_eq!(header.da, CLIENT_MAC_ADDR);
    assert_eq!(header.sa, ETH_DST_MAC);
    assert_eq!(header.ether_type.to_native(), mac::ETHER_TYPE_IPV4);
    assert_eq!(&payload[..], PAYLOAD);
}

/// Test an ethernet device using netdevice backed by WLAN device and send and receive data
/// frames by verifying frames are delivered without any change in both directions.
#[fuchsia_async::run_singlethreaded(test)]
async fn ethernet_tx_rx() {
    init_syslog();

    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let () = loop_until_iface_is_found(&mut helper).await;

    connect_with_security_type(
        &mut helper,
        &AP_SSID,
        &BSS,
        None,
        Protection::Open,
        fidl_policy::SecurityType::None,
    )
    .await;

    let (client, port) = netdevice_helper::create_client(fidl_fuchsia_net::MacAddress {
        octets: CLIENT_MAC_ADDR.clone(),
    })
    .await
    .expect("failed to create netdevice client");
    let (session, _task) = netdevice_helper::start_session(client, port).await;
    verify_tx_and_rx(&session, &port, &mut helper).await;

    helper.stop().await;
}
