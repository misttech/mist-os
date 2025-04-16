// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_bluetooth_hfp as hfp;
use fuchsia_sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use super::commands::Command;

use crate::fidl::call::{Call, CallInfo, LocalCallId};
use crate::fidl::peer::{LocalPeerId, Peer, PeerInfo};

#[allow(unused)]
pub struct CommandHandler {
    peers: Arc<Mutex<HashMap<LocalPeerId, Peer>>>,
    calls: Arc<Mutex<HashMap<LocalCallId, Call>>>,
}

impl CommandHandler {
    pub fn new(
        peers: Arc<Mutex<HashMap<LocalPeerId, Peer>>>,
        calls: Arc<Mutex<HashMap<LocalCallId, Call>>>,
    ) -> Self {
        Self { peers, calls }
    }

    pub async fn handle_command(&mut self, command: Command, args: Vec<&str>) -> Result<(), Error> {
        match command {
            Command::Help => print!("{}", Command::help_msg()),
            Command::ListPeers => self.list_peers(command, args),
            Command::ListCalls => self.list_calls(command, args),
            Command::DialFromNumber => self.dial_from_number(command, args).await,
            Command::DialFromMemoryLocation => self.dial_from_memory_location(command, args).await,
            Command::RedialLast => self.redial_last(command, args).await,
            command => println! {"{command} not implemented!"},
        }
        Ok(())
    }

    fn get_peer_info_by_str_id(&self, str: &str) -> Option<PeerInfo> {
        let Ok(id) = str.parse() else {
            println!("Invalid local peer ID: \"{str}\".");
            return None;
        };

        let peers = self.peers.lock();
        let peer_option = peers.get(&id);

        let Some(peer) = peer_option else {
            println!("No peer with local peer ID {id}.");
            return None;
        };

        Some(peer.info.clone())
    }

    fn get_all_peer_infos(&self) -> Vec<PeerInfo> {
        let peers = self.peers.lock();
        let mut peer_infos: Vec<PeerInfo> =
            peers.iter().map(|id_and_peer| id_and_peer.1.info.clone()).collect();

        peer_infos.sort_by_key(|info| info.local_id);

        peer_infos
    }

    fn get_call_info_by_str_id(&self, str: &str) -> Option<CallInfo> {
        let id_result = str.parse();
        let id = match id_result {
            Err(_) => {
                println!("Invalid call ID: \"{str}\".");
                return None;
            }
            Ok(id) => id,
        };

        let calls = self.calls.lock();
        let call_option = calls.get(&id);

        let call = match call_option {
            None => {
                println!("No call with call ID {id}.");
                return None;
            }
            Some(call) => call,
        };

        Some(call.info.clone())
    }

    fn get_all_call_infos(&self) -> Vec<CallInfo> {
        let calls = self.calls.lock();
        let mut call_infos: Vec<CallInfo> =
            calls.iter().map(|id_and_call| id_and_call.1.info.clone()).collect();

        call_infos.sort_by_key(|info| info.local_id);

        call_infos
    }

    fn list_peers(&mut self, command: Command, args: Vec<&str>) {
        let len = args.len();
        match len {
            0 => {
                let peer_infos = self.get_all_peer_infos();
                for peer_info in peer_infos {
                    println!("{peer_info:?}");
                }
            }
            1 => {
                let str_id = args[0];
                let peer_info_option = self.get_peer_info_by_str_id(str_id);
                if let Some(peer_info) = peer_info_option {
                    println!("{peer_info:?}");
                }
                // Else the errors have already been printed.
            }
            _ => println!("Too many argments for {command}:\n\t{}", command.cmd_help()),
        }
    }

    fn list_calls(&mut self, command: Command, args: Vec<&str>) {
        let len = args.len();
        match len {
            0 => {
                let call_infos = self.get_all_call_infos();
                for call_info in call_infos {
                    println!("{call_info:?}");
                }
            }
            1 => {
                let str_id = args[0];
                let call_info_option = self.get_call_info_by_str_id(str_id);
                if let Some(call_info) = call_info_option {
                    println!("{call_info:?}");
                }
                // Else the errors have already been printed.
            }
            _ => println!("Too many argments for {command}:\n\t{}", command.cmd_help()),
        }
    }

    async fn request_outgoing_call(&self, peer_id_str: &str, call_action: hfp::CallAction) {
        if let Some(peer_info) = self.get_peer_info_by_str_id(peer_id_str) {
            let call_result = peer_info.proxy.request_outgoing_call(&call_action).await;
            match call_result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => println!("HFP error: {err}"),
                Err(err) => println!("FIDL error: {err}"),
            }
        }
        // Else the errors have already been printed.
    }

    async fn dial_from_number(&mut self, command: Command, args: Vec<&str>) {
        let len = args.len();
        match len {
            0 | 1 => println!("Not enough arguments for {command}:\n\t{}", command.cmd_help()),
            2 => {
                let number = String::from(args[1]);
                let call_action = hfp::CallAction::DialFromNumber(number);
                self.request_outgoing_call(/* peer_id = */ args[0], call_action).await
            }
            _ => println!("Too many argments for {command}:\n\t{}", command.cmd_help()),
        }
    }

    async fn dial_from_memory_location(&mut self, command: Command, args: Vec<&str>) {
        let len = args.len();
        match len {
            0 | 1 => println!("Not enough arguments for {command}:\n\t{}", command.cmd_help()),
            2 => {
                let location = String::from(args[1]);
                let call_action = hfp::CallAction::DialFromLocation(location);
                self.request_outgoing_call(/* peer_id = */ args[0], call_action).await
            }
            _ => println!("Too many argments for {command}:\n\t{}", command.cmd_help()),
        }
    }

    async fn redial_last(&mut self, command: Command, args: Vec<&str>) {
        let len = args.len();
        match len {
            0 => println!("Not enough arguments for {command}:\n\t{}", command.cmd_help()),
            1 => {
                let call_action = hfp::CallAction::RedialLast(hfp::RedialLast);
                self.request_outgoing_call(/* peer_id = */ args[0], call_action).await
            }
            _ => println!("Too many argments for {command}:\n\t{}", command.cmd_help()),
        }
    }
}
