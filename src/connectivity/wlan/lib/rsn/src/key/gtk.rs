// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{key::Tk, rsn_ensure, Error};
use mundane::bytes;
use std::hash::{Hash, Hasher};
use wlan_common::ie::rsn::cipher::Cipher;

/// This GTK provider does not support key rotations yet.
#[derive(Debug)]
pub struct GtkProvider {
    key: Box<[u8]>,
    cipher: Cipher,
}

fn generate_random_gtk(len: usize) -> Box<[u8]> {
    let mut key = vec![0; len];
    bytes::rand(&mut key[..]);
    key.into_boxed_slice()
}

impl GtkProvider {
    pub fn new(cipher: Cipher) -> Result<GtkProvider, anyhow::Error> {
        let tk_len: usize =
            cipher.tk_bytes().ok_or(Error::GtkHierarchyUnsupportedCipherError)?.into();
        Ok(GtkProvider { cipher, key: generate_random_gtk(tk_len) })
    }

    pub fn get_gtk(&self) -> Result<Gtk, Error> {
        Gtk::from_bytes(self.key.clone(), self.cipher.clone(), 0, 0)
    }
}

#[derive(Debug, Clone, Eq)]
pub struct Gtk {
    pub bytes: Box<[u8]>,
    cipher: Cipher,
    tk_len: usize,
    key_id: u8,
    key_rsc: u64,
}

/// PartialEq implementation is the same as the default derive(PartialEq)
/// We explicitly implement it here because we have a custom Hash implementation, and clippy
/// requires that both PartialEq and Hash are either derive together or have custom implementations
/// together.
impl PartialEq for Gtk {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
            && self.tk_len == other.tk_len
            && self.key_id == other.key_id
            && self.key_rsc == other.key_rsc
    }
}

/// Custom Hash implementation which doesn't take the RSC or cipher suite into consideration.
/// Make sure to check that this property is upheld: `v1 == v2 => hash(v1) == hash(v2)`
impl Hash for Gtk {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key_id.hash(state);
        self.tk().hash(state);
    }
}

impl Gtk {
    pub fn from_bytes(
        bytes: Box<[u8]>,
        cipher: Cipher,
        key_id: u8,
        key_rsc: u64,
    ) -> Result<Gtk, Error> {
        let tk_len: usize =
            cipher.tk_bytes().ok_or(Error::GtkHierarchyUnsupportedCipherError)?.into();
        rsn_ensure!(bytes.len() >= tk_len, "GTK must be larger than the resulting TK");
        Ok(Gtk { cipher, bytes, tk_len, key_id, key_rsc })
    }

    pub fn cipher(&self) -> &Cipher {
        &self.cipher
    }
    pub fn key_id(&self) -> u8 {
        self.key_id
    }

    pub fn key_rsc(&self) -> u64 {
        self.key_rsc
    }
}

impl Tk for Gtk {
    fn tk(&self) -> &[u8] {
        &self.bytes[0..self.tk_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use wlan_common::ie::rsn::{cipher, suite_selector::OUI};

    #[test]
    fn generated_gtks_are_not_zero_and_not_constant_with_high_probability() {
        let mut gtks = HashSet::new();
        for i in 0..10 {
            let provider = GtkProvider::new(Cipher { oui: OUI, suite_type: cipher::CCMP_128 })
                .expect("failed creating GTK Provider");
            let gtk = provider.get_gtk().expect("could not read GTK").tk().to_vec();
            assert!(gtk.iter().any(|&x| x != 0));
            if i > 0 && !gtks.contains(&gtk) {
                return;
            }
            gtks.insert(gtk);
        }
        panic!("GtkProvider::generate_gtk() generated the same GTK 10 times in a row.");
    }
}
