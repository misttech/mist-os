// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ::async_trait::async_trait;
use ::ffx_bluetooth_peer_args::{PeerCommand, PeerSubCommand};
use ::fho::{
    AvailabilityFlag, Error, FfxMain, FfxTool, FhoEnvironment, Result, TryFromEnv, TryFromEnvWith,
};
use async_utils::hanging_get::client::HangingGetStream;
use ffx_bluetooth_common::PeerIdOrAddr;
use ffx_writer::{SimpleWriter, ToolIO as _};
use fidl_fuchsia_bluetooth::PeerId as FidlPeerId;
use fidl_fuchsia_bluetooth_sys::{AccessProxy, Peer as FidlPeer};
use fuchsia_async::TimeoutExt;
use fuchsia_bluetooth::types::{Address, Peer, PeerId};
use futures::stream::StreamExt;
use prettytable::{cell, format, row, Row, Table};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;
use target_holders::toolbox;

#[derive(FfxTool)]
#[check(AvailabilityFlag("bluetooth.enabled"))]
pub struct PeerTool {
    #[command]
    cmd: PeerCommand,
    peer_watcher_stream: PeerWatcherStream,
    state: State,
    #[with(toolbox())]
    access_proxy: AccessProxy,
}

fho::embedded_plugin!(PeerTool);
#[async_trait(?Send)]
impl FfxMain for PeerTool {
    type Writer = SimpleWriter;
    async fn main(mut self, mut writer: Self::Writer) -> Result<()> {
        let _ = get_known_peers(&mut self).await?;
        match self.cmd.subcommand.clone() {
            // ffx bluetooth peer list
            PeerSubCommand::List(ref cmd) => {
                let args: &[&str] = match cmd.filter.as_ref().map(|s| s.as_str()) {
                    Some(filter) => &[filter],
                    None => &[],
                };
                writer.line(get_peers(args, &self.state, cmd.details))?;
            }
            // ffx bluetooth peer show
            PeerSubCommand::Show(ref cmd) => {
                writer.line(get_peer(&cmd.id_or_addr, self.state))?;
            }
            // ffx bluetooth peer connect
            PeerSubCommand::Connect(ref cmd) => {
                let Some(peer_id) = to_identifier(&self.state, &cmd.id_or_addr) else {
                    return Err(fho::Error::User(anyhow::anyhow!(
                        "Unable to connect: Unknown address {}",
                        cmd.id_or_addr
                    )));
                };

                match connect(&self.access_proxy, peer_id).await {
                    Ok(_) => {
                        writer.line(format!("Successfully connected to peer {peer_id}"))?;
                    }
                    Err(e) => {
                        return Err(fho::Error::User(anyhow::anyhow!(
                            "Failed to connect to peer {}: {}",
                            peer_id,
                            e
                        )));
                    }
                }
            }
            // ffx bluetooth peer disconnect
            PeerSubCommand::Disconnect(ref cmd) => {
                let Some(peer_id) = to_identifier(&self.state, &cmd.id_or_addr) else {
                    return Err(fho::Error::User(anyhow::anyhow!(
                        "Unable to disconnect: Unknown address {}",
                        cmd.id_or_addr
                    )));
                };

                match disconnect(&self.access_proxy, peer_id).await {
                    Ok(_) => {
                        writer.line(format!("Successfully disconnected from peer {peer_id}"))?;
                    }
                    Err(e) => {
                        return Err(fho::Error::User(anyhow::anyhow!(
                            "Failed to disconnect from peer {}: {}",
                            peer_id,
                            e
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Disconnects an active BR/EDR or LE connection by input peer ID.
pub async fn disconnect(access: &AccessProxy, id: PeerId) -> Result<(), Error> {
    let fidl_peer_id: FidlPeerId = id.into();
    let _ = access.disconnect(&fidl_peer_id).await.map_err(|e| {
        let user_err = anyhow::anyhow!(format!("Disconnect error: {:?}", e));
        fho::Error::User(user_err)
    })?;
    Ok(())
}

/// Connects over BR/EDR or LE to an input peer ID.
async fn connect(access: &AccessProxy, id: PeerId) -> Result<(), fho::Error> {
    let fidl_peer_id: FidlPeerId = id.into();
    let _ = access.connect(&fidl_peer_id).await.map_err(|e| {
        let user_err = anyhow::anyhow!(format!("Connect error: {:?}", e));
        fho::Error::User(user_err)
    })?;
    Ok(())
}

/// Get the string representation of a peer from either an identifier or address
fn get_peer<'a>(key: &PeerIdOrAddr, state: State) -> String {
    to_identifier(&state, &key)
        .and_then(|id| state.peers.get(&id).map(|peer| peer.to_string()))
        .unwrap_or_else(|| String::from("No known peer"))
}

fn get_peers<'a>(args: &'a [&'a str], state: &State, full_details: bool) -> String {
    let find = args.first().unwrap_or(&"");

    if state.peers.is_empty() {
        return String::from("No known peers");
    }
    let mut peers: Vec<&Peer> = state.peers.values().filter(|p| match_peer(&find, p)).collect();
    peers.sort_by(|a, b| cmp_peers(&*a, &*b));
    let matched = format!("Showing {}/{} peers\n", peers.len(), state.peers.len());

    if full_details {
        return String::from_iter(
            std::iter::once(matched).chain(peers.iter().map(|p| p.to_string())),
        );
    }

    // Create table of results
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER);
    let _ = table.set_titles(row![
        "PeerId",
        "Address",
        "Technology",
        "Name",
        "Appearance",
        "Connected",
        "Bonded",
    ]);
    for val in peers.into_iter() {
        let _ = table.add_row(peer_to_table_row(val));
    }
    [matched, format!("{}", table)].join("\n")
}

fn match_peer<'a>(pattern: &'a str, peer: &Peer) -> bool {
    let pattern_upper = &pattern.to_uppercase();
    peer.id.to_string().to_uppercase().contains(pattern_upper)
        || peer.address.to_string().to_uppercase().contains(pattern_upper)
        || peer.name.as_ref().is_some_and(|p| p.contains(pattern))
}

/// Order connected peers as greater than unconnected peers and bonded peers greater than unbonded
/// peers.
fn cmp_peers(a: &Peer, b: &Peer) -> Ordering {
    (a.connected, a.bonded).cmp(&(b.connected, b.bonded))
}

/// Returns basic peer information formatted as a prettytable Row
fn peer_to_table_row(peer: &Peer) -> Row {
    let addr_hex = peer.address.as_hex_string();
    let addr_short = match peer.address {
        Address::Public(_) => format!("public {addr_hex}"),
        Address::Random(_) => format!("random {addr_hex}"),
    };
    row![
        peer.id.to_string(),
        addr_short,
        format! {"{:?}", peer.technology},
        peer.name.as_ref().map_or("".to_string(), |x| format!("{:?}", x)),
        peer.appearance.as_ref().map_or("".to_string(), |x| format!("{:?}", x)),
        peer.connected.to_string(),
        peer.bonded.to_string(),
    ]
}

// Find the identifier for a `Peer` based on a `key` that is either an identifier or an address.
// Returns `None` if the given address does not belong to a known peer.
fn to_identifier(state: &State, key: &PeerIdOrAddr) -> Option<PeerId> {
    match key {
        PeerIdOrAddr::PeerId(id) => Some(*id),
        PeerIdOrAddr::BdAddr(addr) => state
            .peers
            .values()
            .find(|peer| peer.address.as_hex_string() == addr.0)
            .map(|peer| peer.id),
    }
}

async fn get_known_peers(peer_tool: &mut PeerTool) -> Result<HashMap<PeerId, Peer>, Error> {
    loop {
        let Some(stream) = &mut peer_tool.peer_watcher_stream.peer_watcher_stream else {
            break Err(fho::Error::Unexpected(anyhow::anyhow!(
                "Peer Watcher Stream not available"
            )));
        };

        let Some(res) = stream.next().on_timeout(Duration::from_millis(50), || None).await else {
            break Ok(peer_tool.state.peers.clone());
        };

        match res {
            Ok(d) => {
                let (discovered_peers, removed_peers) = d;

                for peer_id in removed_peers {
                    let peer_id = PeerId(peer_id.value);
                    if peer_tool.state.peers.contains_key(&peer_id) {
                        peer_tool.state.peers.remove(&peer_id);
                    }
                }

                let peers_iter = discovered_peers.iter().map(|d| {
                    let peer: Peer =
                        Peer::try_from(d.clone()).expect("Failed to convert FidlPeer to Peer");
                    (PeerId(d.id.unwrap().value), peer)
                });

                peer_tool.state.peers.extend(peers_iter);
            }
            Err(e) => {
                break Err(fho::Error::Unexpected(anyhow::anyhow!(
                    "Peer Watcher Stream failed with: {:?}",
                    e
                )));
            }
        }
    }
}

pub struct PeerWatcherStream {
    pub peer_watcher_stream:
        Option<HangingGetStream<AccessProxy, (Vec<FidlPeer>, Vec<FidlPeerId>)>>,
}

#[async_trait(?Send)]
impl TryFromEnv for PeerWatcherStream {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let access_proxy = toolbox::<AccessProxy>().try_from_env_with(env).await?;
        let stream = HangingGetStream::new_with_fn_ptr(access_proxy, AccessProxy::watch_peers);
        Ok(PeerWatcherStream { peer_watcher_stream: Some(stream) })
    }
}

/// Tracks all state local to the command line tool.
#[derive(Clone, Debug)]
pub struct State {
    pub peers: HashMap<PeerId, Peer>,
}

impl State {
    pub fn new() -> State {
        State { peers: HashMap::new() }
    }
}

#[async_trait(?Send)]
impl TryFromEnv for State {
    async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
        Ok(State::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_bluetooth_common::{BdAddr, PeerIdOrAddr};
    use fuchsia_bluetooth::types::Address;
    use regex::Regex;
    use std::str::FromStr;
    use {fidl_fuchsia_bluetooth as fbt, fidl_fuchsia_bluetooth_sys as fsys};

    fn named_peer(id: PeerId, address: Address, name: Option<String>) -> Peer {
        Peer {
            id,
            address,
            technology: fsys::TechnologyType::LowEnergy,
            connected: false,
            bonded: false,
            name,
            appearance: Some(fbt::Appearance::Phone),
            device_class: None,
            rssi: None,
            tx_power: None,
            le_services: vec![],
            bredr_services: vec![],
        }
    }

    fn custom_peer(
        id: PeerId,
        address: Address,
        connected: bool,
        bonded: bool,
        rssi: Option<i8>,
    ) -> Peer {
        Peer {
            id,
            address,
            technology: fsys::TechnologyType::LowEnergy,
            connected,
            bonded,
            name: None,
            appearance: Some(fbt::Appearance::Phone),
            device_class: None,
            rssi,
            tx_power: None,
            le_services: vec![],
            bredr_services: vec![],
        }
    }

    #[fuchsia::test]
    fn test_match_peer() {
        let nameless_peer =
            named_peer(PeerId(0xabcd), Address::Public([0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]), None);
        let named_peer = named_peer(
            PeerId(0xbeef),
            Address::Public([0x11, 0x00, 0x55, 0x7E, 0xDE, 0xAD]),
            Some("Sapphire".to_string()),
        );

        assert!(match_peer("23", &nameless_peer));
        assert!(!match_peer("23", &named_peer));

        assert!(match_peer("cd", &nameless_peer));
        assert!(match_peer("bee", &named_peer));
        assert!(match_peer("BEE", &named_peer));

        assert!(!match_peer("Sapphire", &nameless_peer));
        assert!(match_peer("Sapphire", &named_peer));

        assert!(match_peer("", &nameless_peer));
        assert!(match_peer("", &named_peer));

        assert!(match_peer("DE", &named_peer));
        assert!(match_peer("de", &named_peer));
    }

    #[test]
    fn test_get_peers_full_details() {
        let mut state = State::new();
        let _ = state.peers.insert(
            PeerId(0xabcd),
            named_peer(PeerId(0xabcd), Address::Public([0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]), None),
        );
        let _ = state.peers.insert(
            PeerId(0xbeef),
            named_peer(
                PeerId(0xbeef),
                Address::Public([0x11, 0x00, 0x55, 0x7E, 0xDE, 0xAD]),
                Some("Sapphire".to_string()),
            ),
        );

        let get_peers = |args: &[&str], state: &State| -> String { get_peers(args, state, true) };

        // Fields for detailed view of peers
        let fields = Regex::new(r"Id(?s).*Address(?s).*Technology(?s).*Name(?s).*Appearance(?s).*Connected(?s).*Bonded(?s).*LE Services(?s).*BR/EDR Serv\.").unwrap();

        // Empty arguments matches everything
        assert!(fields.is_match(&get_peers(&[], &state)));
        assert!(get_peers(&[], &state).contains("2/2 peers"));
        assert!(get_peers(&[], &state).contains("01:23:45"));
        assert!(get_peers(&[], &state).contains("AD:DE:7E"));

        // No matches prints nothing.
        assert!(!fields.is_match(&get_peers(&["nomatch"], &state)));
        assert!(get_peers(&["nomatch"], &state).contains("0/2 peers"));
        assert!(!get_peers(&["nomatch"], &state).contains("01:23:45"));
        assert!(!get_peers(&["nomatch"], &state).contains("AD:DE:7E"));

        // We can match either one
        assert!(get_peers(&["01:23"], &state).contains("1/2 peers"));
        assert!(get_peers(&["01:23"], &state).contains("01:23:45"));
        assert!(get_peers(&["abcd"], &state).contains("1/2 peers"));
        assert!(get_peers(&["beef"], &state).contains("AD:DE:7E"));
    }

    #[test]
    fn test_get_peers_less_details() {
        let mut state = State::new();
        let _ = state.peers.insert(
            PeerId(0xabcd),
            named_peer(PeerId(0xabcd), Address::Public([0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]), None),
        );
        let _ = state.peers.insert(
            PeerId(0xbeef),
            named_peer(
                PeerId(0xbeef),
                Address::Public([0x11, 0x00, 0x55, 0x7E, 0xDE, 0xAD]),
                Some("Sapphire".to_string()),
            ),
        );

        let get_peers = |args: &[&str], state: &State| -> String { get_peers(args, state, false) };

        // Fields for table view of peers
        let fields = Regex::new(r"PeerId[ \t]*\|[ \t]*Address[ \t]*\|[ \t]*Technology[ \t]*\|[ \t]*Name[ \t]*\|[ \t]*Appearance[ \t]*\|[ \t]*Connected[ \t]*\|[ \t]*Bonded").unwrap();

        // Empty arguments matches everything
        assert!(fields.is_match(&get_peers(&[], &state)));
        assert!(get_peers(&[], &state).contains("2/2 peers"));
        assert!(get_peers(&[], &state).contains("01:23:45"));
        assert!(get_peers(&[], &state).contains("AD:DE:7E"));

        // No matches prints nothing.
        assert!(!fields.is_match(&get_peers(&["nomatch"], &state)));
        assert!(get_peers(&["nomatch"], &state).contains("0/2 peers"));
        assert!(!get_peers(&["nomatch"], &state).contains("01:23:45"));
        assert!(!get_peers(&["nomatch"], &state).contains("AD:DE:7E"));

        // We can match either one
        assert!(get_peers(&["01:23"], &state).contains("1/2 peers"));
        assert!(get_peers(&["01:23"], &state).contains("01:23:45"));
        assert!(get_peers(&["abcd"], &state).contains("1/2 peers"));
        assert!(get_peers(&["beef"], &state).contains("AD:DE:7E"));
    }

    #[test]
    fn cmp_peers_correctly_orders_peers() {
        // Sorts connected correctly
        let peer_a =
            custom_peer(PeerId(0xbeef), Address::Public([1, 0, 0, 0, 0, 0]), false, false, None);
        let peer_b =
            custom_peer(PeerId(0xbaaf), Address::Public([2, 0, 0, 0, 0, 0]), true, false, None);
        assert_eq!(cmp_peers(&peer_a, &peer_b), Ordering::Less);

        // Sorts bonded correctly
        let peer_a =
            custom_peer(PeerId(0xbeef), Address::Public([1, 0, 0, 0, 0, 0]), false, false, None);
        let peer_b =
            custom_peer(PeerId(0xbaaf), Address::Public([2, 0, 0, 0, 0, 0]), false, true, None);
        assert_eq!(cmp_peers(&peer_a, &peer_b), Ordering::Less);
    }

    #[test]
    fn test_get_peer() {
        let mut state = State::new();

        let peer_id1 = PeerId(0xabcd);
        let address1 = Address::Public([0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]);
        let peer1 = named_peer(peer_id1, address1, Some("Sapphire".to_string()));
        let _ = state.peers.insert(peer_id1, peer1.clone());

        let peer_id2 = PeerId(0xbeef);
        let address2 = Address::Public([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let peer2 = named_peer(peer_id2, address2, None);
        let _ = state.peers.insert(peer_id2, peer2.clone());

        // Valid ID
        let result =
            get_peer(&PeerIdOrAddr::from_str(&peer_id1.to_string()).unwrap(), state.clone());
        assert_eq!(result, peer1.to_string());

        // Valid Address
        let result =
            get_peer(&PeerIdOrAddr::from_str(&address2.as_hex_string()).unwrap(), state.clone());
        assert_eq!(result, peer2.to_string());

        // Invalid ID
        let invalid_peer_id = PeerId(0x1234);
        let result =
            get_peer(&PeerIdOrAddr::from_str(&invalid_peer_id.to_string()).unwrap(), state.clone());
        assert_eq!(result, "No known peer");

        // Invalid Address Format
        let result = get_peer(
            &PeerIdOrAddr::from_str("invalid_address_format")
                .unwrap_or(PeerIdOrAddr::PeerId(PeerId(0))),
            state.clone(),
        );
        assert_eq!(result, "No known peer");

        // Empty State
        let empty_state = State::new();
        let result = get_peer(&PeerIdOrAddr::from_str(&peer_id1.to_string()).unwrap(), empty_state);
        assert_eq!(result, "No known peer");
    }

    #[test]
    fn test_to_identifier() {
        let mut state = State::new();
        let peer_id1 = PeerId(0xabcd);
        let address1 = Address::Public([0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]);
        let peer1 = named_peer(peer_id1, address1.clone(), Some("Sapphire".to_string()));
        state.peers.insert(peer_id1, peer1);

        let peer_id2 = PeerId(0xbeef);
        let address2 = Address::Public([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let peer2 = named_peer(peer_id2, address2.clone(), None);
        state.peers.insert(peer_id2, peer2);

        // Valid ID Input
        let result = to_identifier(&state, &PeerIdOrAddr::PeerId(peer_id1));
        assert_eq!(result, Some(peer_id1));

        // Valid Address Input
        let bd_addr2 = PeerIdOrAddr::BdAddr(BdAddr(address2.as_hex_string()));
        let result = to_identifier(&state, &bd_addr2);
        assert_eq!(result, Some(peer_id2));

        // Invalid Address Input
        let invalid_address = PeerIdOrAddr::BdAddr(BdAddr("00:00:00:00:00:00".to_string()));
        let result = to_identifier(&state, &invalid_address);
        assert_eq!(result, None);

        // Invalid Address Format
        let invalid_format = PeerIdOrAddr::BdAddr(BdAddr("invalid-format".to_string()));
        let result = to_identifier(&state, &invalid_format);
        assert_eq!(result, None);

        // Empty State
        let empty_state = State::new();
        let bd_addr1 = PeerIdOrAddr::BdAddr(BdAddr(address1.as_hex_string()));
        let result = to_identifier(&empty_state, &bd_addr1);
        assert_eq!(result, None);
    }
}
