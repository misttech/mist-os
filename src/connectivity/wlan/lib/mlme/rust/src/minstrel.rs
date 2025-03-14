// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::mac::WlanGi;
use crate::probe_sequence::{ProbeEntry, ProbeSequence};
use ieee80211::{MacAddr, MacAddrBytes};
use log::{debug, error};
use std::collections::{hash_map, HashMap, HashSet};
use std::time::Duration;
use wlan_common::ie::{HtCapabilities, RxMcsBitmask, SupportedRate};
use wlan_common::mac::FrameControl;
use wlan_common::tx_vector::{
    TxVecIdx, TxVector, ERP_NUM_TX_VECTOR, ERP_START_IDX, HT_NUM_MCS, HT_NUM_UNIQUE_MCS,
};
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_minstrel as fidl_minstrel,
    fidl_fuchsia_wlan_softmac as fidl_softmac,
};

// TODO(https://fxbug.dev/42103418): Enable CBW40 support once its information is available from AssocCtx.
const ASSOC_CHAN_WIDTH: fidl_common::ChannelBandwidth = fidl_common::ChannelBandwidth::Cbw20;

const MCS_MASK_0_31: u128 = 0xFFFFFFFF;
const MINSTREL_FRAME_LENGTH: u32 = 1400; // bytes
const MINSTREL_EXP_WEIGHT: f32 = 0.75; // Used to calculate moving average throughput

// TODO(https://fxbug.dev/42163096): Determine if we should use a separate variable for (1.0 - MINSTREL_PROABILITY_THRESHOLD).
const MINSTREL_PROBABILITY_THRESHOLD: f32 = 0.9; // If probability is past this level,
                                                 // only consider throughput
const PROBE_INTERVAL: u8 = 16; // Number of normal packets to send between two probe packets.
const MAX_SLOW_PROBE: u64 = 2; // If the data rate is low, don't probe more than twice per update interval.
const DEAD_PROBE_CYCLE_COUNT: u8 = 32; // If the success rate is under (1 - MINSTREL_PROBABILITY_THRESHOLD)
                                       // only probe once every DEAD_PROBE_CYCLE_COUNT cycles.

type TxStatsMap = HashMap<TxVecIdx, TxStats>;

struct TxStats {
    tx_vector_idx: TxVecIdx,
    perfect_tx_time: Duration, // Minimum possible time to transmit a MINSTREL_FRAME_LENGTH frame at this rate.
    success_cur: u64,          // Successful transmissions since last update.
    attempts_cur: u64,         // Transmission attempts since last update.
    moving_avg_probability: f32, // Exponentially weighted moving average probability of success.
    expected_throughput: f32,  // Expected average throughput.
    success_total: u64,        // Cumulative success counts. u64 to avoid overflow issues.
    attempts_total: u64,       // Cumulative attempts. u64 to avoid overflow issues.
    probe_cycles_skipped: u8, // Reduce probe frequency if probability < (1 - MINSTREL_PROBABILITY_THRESHOLD)
    probes_total: u64,
}

impl TxStats {
    fn new(tx_vector_idx: TxVecIdx) -> Self {
        Self {
            tx_vector_idx,
            perfect_tx_time: Duration::ZERO,
            success_cur: 0,
            attempts_cur: 0,
            // A low initial probability of success, but high enough to avoid marking the rate dead.
            moving_avg_probability: 1.0 - MINSTREL_PROBABILITY_THRESHOLD,
            expected_throughput: 0.0,
            success_total: 0,
            attempts_total: 0,
            probe_cycles_skipped: 0,
            probes_total: 0,
        }
    }

    fn phy_type_strictly_preferred_over(&self, other: &TxStats) -> bool {
        // Based on experiments, if HT is supported, it is better not to use ERP for
        // data frames. With ralink RT5592 and Netgear Nighthawk X10, approximately 80
        // feet away, HT/ERP tx throughput < 1 Mbps, HT only tx 4-8 Mbps
        // TODO(https://fxbug.dev/42104244): Revisit with VHT support.
        self.tx_vector_idx.is_ht() && !other.tx_vector_idx.is_ht()
    }

    fn better_for_expected_throughput_than(&self, other: &TxStats) -> bool {
        (self.expected_throughput, self.moving_avg_probability)
            > (other.expected_throughput, other.moving_avg_probability)
    }

    fn better_for_reliable_transmission_than(&self, other: &TxStats) -> bool {
        if self.moving_avg_probability > MINSTREL_PROBABILITY_THRESHOLD
            && other.moving_avg_probability > MINSTREL_PROBABILITY_THRESHOLD
        {
            // When probability is high enough, consider throughput instead.
            self.expected_throughput > other.expected_throughput
        } else {
            self.moving_avg_probability > other.moving_avg_probability
        }
    }
}

impl From<&TxStats> for fidl_minstrel::StatsEntry {
    fn from(stats: &TxStats) -> fidl_minstrel::StatsEntry {
        fidl_minstrel::StatsEntry {
            tx_vector_idx: *stats.tx_vector_idx,
            tx_vec_desc: format!("{}", stats.tx_vector_idx),
            success_cur: stats.success_cur,
            attempts_cur: stats.attempts_cur,
            probability: stats.moving_avg_probability,
            cur_tp: stats.expected_throughput,
            success_total: stats.success_total,
            attempts_total: stats.attempts_total,
            probes_total: stats.probes_total,
            probe_cycles_skipped: stats.probe_cycles_skipped,
        }
    }
}

struct Peer {
    addr: MacAddr,
    tx_stats_map: TxStatsMap,
    erp_rates: HashSet<TxVecIdx>,
    highest_erp_rate: Option<TxVecIdx>,         // Set by AssocCtx
    best_erp_for_reliability: Option<TxVecIdx>, // Based on transmission sucess probability for erp rates
    best_expected_throughput: Option<TxVecIdx>, // Based on expected throughput
    best_for_reliability: Option<TxVecIdx>,     // Based on transmission success probability

    // Probe parameters
    num_probe_cycles_done: u64,
    num_pkt_until_next_probe: u8,
    probes_total: u64,
    probe_entry: ProbeEntry,
}

impl Peer {
    fn from_assoc_cfg(assoc_cfg: &fidl_softmac::WlanAssociationConfig) -> Result<Self, zx::Status> {
        let mut peer = Self {
            addr: match assoc_cfg.bssid {
                None => {
                    error!("Cannot create Peer without a BSSID.");
                    return Err(zx::Status::INTERNAL);
                }
                Some(bssid) => bssid.into(),
            },
            num_pkt_until_next_probe: PROBE_INTERVAL - 1,
            tx_stats_map: Default::default(),
            erp_rates: Default::default(),
            highest_erp_rate: Default::default(),
            best_erp_for_reliability: Default::default(),
            best_expected_throughput: Default::default(),
            best_for_reliability: Default::default(),
            num_probe_cycles_done: Default::default(),
            probes_total: Default::default(),
            probe_entry: Default::default(),
        };

        if let Some(ht_cap) = assoc_cfg.ht_cap {
            let mut ht_cap = HtCapabilities::from(ht_cap.clone());

            // TODO(https://fxbug.dev/42104244): SGI support suppressed. Remove these once they are supported.
            let mut cap_info = ht_cap.ht_cap_info;
            cap_info.set_short_gi_20(false);
            cap_info.set_short_gi_40(false);
            ht_cap.ht_cap_info = cap_info;

            let mcs_set = ht_cap.mcs_set;
            if (mcs_set.rx_mcs().0 & MCS_MASK_0_31) == 0 {
                error!("Invalid AssocCtx. HT supported but no valid MCS: {:?}", mcs_set);
                return Err(zx::Status::INTERNAL);
            } else {
                peer.add_ht(&ht_cap);
            }
        }

        if let Some(rates) = &assoc_cfg.rates {
            peer.erp_rates = peer.add_supported_erp(&rates[..]);
            peer.highest_erp_rate = peer.erp_rates.iter().cloned().max();
        }
        debug!("tx_stats_map populated. size: {}", peer.tx_stats_map.len());
        if peer.tx_stats_map.is_empty() {
            error!("No usable rates for peer {}", &peer.addr);
            return Err(zx::Status::INTERNAL);
        }

        Ok(peer)
    }

    fn handle_tx_result_report(&mut self, tx_result: &fidl_common::WlanTxResult) {
        let mut last_attempted_idx = None;
        for status_entry in &tx_result.tx_result_entry[..] {
            let idx = match TxVecIdx::new(status_entry.tx_vector_idx) {
                Some(idx) => idx,
                None => break,
            };
            last_attempted_idx.replace(idx);
            // Get the stats for this rate, or attempt to add stats if none exist.
            let stats = match self.tx_stats_map.entry(idx) {
                hash_map::Entry::Occupied(val) => Some(val.into_mut()),
                hash_map::Entry::Vacant(vacant) => {
                    idx.to_erp_rate().map(|rate| vacant.insert(erp_idx_stats(idx, rate)))
                }
            };
            match stats {
                Some(stats) => stats.attempts_cur += status_entry.attempts as u64,
                None => {
                    last_attempted_idx.take();
                    debug!("error: Invalid TxVecIdx: {:?}", idx)
                }
            }
        }
        if let Some(idx) = last_attempted_idx {
            if tx_result.result_code == fidl_common::WlanTxResultCode::Success {
                // last_attempted_idx will always have a corresponding tx_stats_map entry.
                self.tx_stats_map.get_mut(&idx).unwrap().success_cur += 1;
            }
        }
    }

    fn add_ht(&mut self, ht_cap: &HtCapabilities) {
        let mut max_size = HT_NUM_MCS + ERP_NUM_TX_VECTOR; // Account for ERP rates.
        let cap_info = ht_cap.ht_cap_info;
        let sgi_20 = cap_info.short_gi_20();
        let sgi_40 = cap_info.short_gi_40();
        if sgi_20 {
            max_size += HT_NUM_MCS;
        }
        if ASSOC_CHAN_WIDTH == fidl_common::ChannelBandwidth::Cbw40 {
            max_size += HT_NUM_MCS;
            if sgi_40 {
                max_size += HT_NUM_MCS;
            }
        }

        debug!("max_size is {}", max_size);
        self.tx_stats_map.reserve(max_size as usize);
        let mcs_set = ht_cap.mcs_set;
        self.add_supported_ht(
            fidl_common::ChannelBandwidth::Cbw20,
            WlanGi::G_800NS,
            mcs_set.rx_mcs(),
        );
        if sgi_20 {
            self.add_supported_ht(
                fidl_common::ChannelBandwidth::Cbw20,
                WlanGi::G_400NS,
                mcs_set.rx_mcs(),
            );
        }
        if ASSOC_CHAN_WIDTH == fidl_common::ChannelBandwidth::Cbw40 {
            self.add_supported_ht(
                fidl_common::ChannelBandwidth::Cbw40,
                WlanGi::G_800NS,
                mcs_set.rx_mcs(),
            );
            if sgi_40 {
                self.add_supported_ht(
                    fidl_common::ChannelBandwidth::Cbw40,
                    WlanGi::G_400NS,
                    mcs_set.rx_mcs(),
                );
            }
        }

        debug!("TxStatsMap size: {}", self.tx_stats_map.len());
    }

    // SupportedMcsRx is 78 bits long in IEEE802.11-2016, Figure 9-334
    // In reality, devices implement MCS 0-31, sometimes 32, almost never beyond 32.
    fn add_supported_ht(
        &mut self,
        channel_bandwidth: fidl_common::ChannelBandwidth,
        gi: WlanGi,
        mcs_set: RxMcsBitmask,
    ) {
        let mut tx_stats_added = 0;
        for mcs_idx in 0..HT_NUM_MCS {
            if mcs_set.support(mcs_idx) {
                let tx_vector =
                    TxVector::new(fidl_common::WlanPhyType::Ht, gi, channel_bandwidth, mcs_idx)
                        .expect("Should be a valid TxVector");
                let tx_vector_idx = tx_vector.to_idx();
                let perfect_tx_time = tx_time_ht(channel_bandwidth, gi, mcs_idx);
                let tx_stats = TxStats { perfect_tx_time, ..TxStats::new(tx_vector_idx) };
                self.tx_stats_map.insert(tx_vector_idx, tx_stats);
                tx_stats_added += 1;
            }
        }
        debug!(
            "{} HTs added with channel_bandwidth={:?}, gi={:?}",
            tx_stats_added, channel_bandwidth, gi
        );
    }

    fn add_supported_erp(&mut self, rates: &[u8]) -> HashSet<TxVecIdx> {
        let mut tx_stats_added = 0;
        let basic_rates: HashSet<TxVecIdx> = rates
            .iter()
            .filter_map(|rate| {
                let rate = SupportedRate(*rate);
                let tx_vector = match TxVector::from_supported_rate(&rate) {
                    Ok(tx_vector) => Some(tx_vector),
                    Err(e) => {
                        error!("Could not create tx vector from supported rate: {}", e);
                        None
                    }
                }?;
                if tx_vector.phy() != fidl_common::WlanPhyType::Erp {
                    return None;
                }
                let tx_vector_idx = tx_vector.to_idx();
                self.tx_stats_map.insert(tx_vector_idx, erp_idx_stats(tx_vector_idx, rate));
                tx_stats_added += 1;
                if rate.basic() {
                    Some(tx_vector_idx)
                } else {
                    None
                }
            })
            .collect();
        debug!("{} ERP added.", tx_stats_added);
        if basic_rates.is_empty() {
            vec![TxVecIdx::new(ERP_START_IDX).unwrap()].into_iter().collect()
        } else {
            basic_rates
        }
    }

    fn update_stats(&mut self) {
        // Update all TxStats for known TxVecIdx.
        for tx_stats in self.tx_stats_map.values_mut() {
            if tx_stats.attempts_cur != 0 {
                let probability = tx_stats.success_cur as f32 / tx_stats.attempts_cur as f32;
                if tx_stats.attempts_total == 0 {
                    tx_stats.moving_avg_probability = probability;
                } else {
                    tx_stats.moving_avg_probability = tx_stats.moving_avg_probability
                        * MINSTREL_EXP_WEIGHT
                        + probability * (1.0 - MINSTREL_EXP_WEIGHT);
                }
                tx_stats.attempts_total += tx_stats.attempts_cur;
                tx_stats.success_total += tx_stats.success_cur;
                tx_stats.attempts_cur = 0;
                tx_stats.success_cur = 0;
                tx_stats.probe_cycles_skipped = 0;
            } else {
                tx_stats.probe_cycles_skipped = tx_stats.probe_cycles_skipped.saturating_add(1);
            }
            const NANOS_PER_SECOND: f32 = 1e9;
            // perfect_tx_time is always non-zero as guaranteed by add_supported_ht and add_supported_erp.
            tx_stats.expected_throughput = NANOS_PER_SECOND
                / tx_stats.perfect_tx_time.as_nanos() as f32
                * tx_stats.moving_avg_probability;
        }

        // Pick a random rate to start comparisons.
        let arbitrary_rate = match self.tx_stats_map.iter().next() {
            Some((tx_vec_idx, _)) => *tx_vec_idx,
            None => return, // There are no supported rates, so everything past here is pointless.
        };

        // Determine optimal data rates for throughput and reliability based on current network conditions.
        let mut best_expected_throughput = arbitrary_rate;
        let mut best_for_reliability = arbitrary_rate;
        let mut best_erp_for_reliability = self.highest_erp_rate;
        for (tx_vector_idx, tx_stats) in &self.tx_stats_map {
            // Unwraps are safe since max_tp and max_probability are taken from
            // iterating through tx_stats_map keys.
            let best_throughput_stats = self.tx_stats_map.get(&best_expected_throughput).unwrap();
            let best_reliability_stats = self.tx_stats_map.get(&best_for_reliability).unwrap();

            // Pick the data rate with the highest throughput. Prefer a better phy type (i.e. HT > ERP),
            // unless the better phy type has extremely poor performance.
            if (!tx_unlikely(tx_stats)
                && tx_stats.phy_type_strictly_preferred_over(best_throughput_stats))
                || (tx_stats.better_for_expected_throughput_than(best_throughput_stats)
                    && !(!tx_unlikely(best_throughput_stats)
                        && best_throughput_stats.phy_type_strictly_preferred_over(tx_stats)))
            {
                best_expected_throughput = *tx_vector_idx;
            }
            // Pick the data rate with the highest probability of transmission success. Prefer a better
            // phy type (i.e. HT > ERP), unless the better phy type has extremely poor performance.
            if (!tx_unlikely(tx_stats)
                && tx_stats.phy_type_strictly_preferred_over(best_reliability_stats))
                || (tx_stats.better_for_reliable_transmission_than(best_reliability_stats)
                    && !(!tx_unlikely(best_reliability_stats)
                        && best_reliability_stats.phy_type_strictly_preferred_over(tx_stats)))
            {
                best_for_reliability = *tx_vector_idx;
            }
            if let Some(best_erp_for_reliability) = best_erp_for_reliability.as_mut() {
                let best_erp_reliability_stats =
                    self.tx_stats_map.get(best_erp_for_reliability).unwrap();
                if self.erp_rates.contains(tx_vector_idx)
                    && tx_stats.better_for_reliable_transmission_than(best_erp_reliability_stats)
                {
                    *best_erp_for_reliability = *tx_vector_idx;
                }
            }
        }
        self.best_expected_throughput = Some(best_expected_throughput);
        self.best_for_reliability = Some(best_for_reliability);
        self.best_erp_for_reliability = best_erp_for_reliability;
    }

    fn get_tx_vector_idx(
        &mut self,
        needs_reliability: bool,
        probe_sequence: &ProbeSequence,
    ) -> Option<TxVecIdx> {
        if needs_reliability {
            self.best_for_reliability
        } else if self.num_pkt_until_next_probe > 0 {
            self.num_pkt_until_next_probe -= 1;
            self.best_expected_throughput
        } else {
            self.num_pkt_until_next_probe = PROBE_INTERVAL - 1;
            self.get_next_probe(probe_sequence)
        }
    }

    fn get_next_probe(&mut self, probe_sequence: &ProbeSequence) -> Option<TxVecIdx> {
        // We generally don't have any reason to switch to a rate with a lower throughput than
        // our most reliable rate. Limit the number of probes for any rate below this threshold.
        let slow_probe_cutoff = self.tx_stats_map.get(&self.best_for_reliability?)?.perfect_tx_time;
        if self.tx_stats_map.len() == 1 {
            return self.best_expected_throughput;
        }
        // Check each entry in the map once.
        for _ in 0..self.tx_stats_map.len() {
            let probe_idx = self.next_supported_probe_idx(probe_sequence);
            let tx_stats = self.tx_stats_map.get_mut(&probe_idx).unwrap();
            // Don't bother probing our current default indices, since we get data on them with every
            // non-probe frame.
            if Some(probe_idx) == self.best_erp_for_reliability.or(self.highest_erp_rate)
                || Some(probe_idx) == self.best_expected_throughput.or(self.best_for_reliability)
                // Don't probe the most-probed index.
                || tx_stats.attempts_cur > self.num_probe_cycles_done
                // Low throughput index, probe at most MAX_SLOW_PROBE times per update interval.
                || (tx_stats.perfect_tx_time > slow_probe_cutoff
                    && tx_stats.attempts_cur >= MAX_SLOW_PROBE)
                // Low probability of success, only probe occasionally.
                || (tx_unlikely(tx_stats)
                    && (tx_stats.probe_cycles_skipped < DEAD_PROBE_CYCLE_COUNT
                        || tx_stats.attempts_cur > 0))
            {
                continue;
            }
            self.probes_total += 1;
            tx_stats.probes_total += 1;
            return Some(probe_idx);
        }
        return self.best_expected_throughput;
    }

    // This is not safe to call unless tx_stats_map.len() > 0
    fn next_supported_probe_idx(&mut self, probe_sequence: &ProbeSequence) -> TxVecIdx {
        assert!(
            !self.tx_stats_map.is_empty(),
            "Cannot call next_supported_probe_idx with empty tx_stats_map"
        );

        loop {
            let idx = probe_sequence.next(&mut self.probe_entry);
            if self.probe_entry.cycle_complete() {
                self.num_probe_cycles_done += 1;
            }
            if self.tx_stats_map.contains_key(&idx) {
                return idx;
            }
        }
    }
}

fn tx_unlikely(tx_stats: &TxStats) -> bool {
    tx_stats.moving_avg_probability < 1.0 - MINSTREL_PROBABILITY_THRESHOLD
}

pub trait TimerManager {
    fn schedule(&mut self, from_now: Duration);
    fn cancel(&mut self);
}

/// MinstrelRateSelector is responsible for handling data rate selection on frame transmission for
/// SoftMAC WLAN interfaces. It stores observed behavior for various compatible data rates,
/// and uses intermittent probes to ensure that we continue to use the data rates with
/// highest throughput and transmission success rate.
///
/// Some SoftMAC devices may provide their own rate selection implementations. In these cases,
/// Minstrel may be used to collect and forward data rate statistics to upper stack layers.
pub struct MinstrelRateSelector<T: TimerManager> {
    timer_manager: T,
    update_interval: Duration,
    probe_sequence: ProbeSequence,
    peer_map: HashMap<MacAddr, Peer>,
    outdated_peers: HashSet<MacAddr>,
}

impl<T: TimerManager> MinstrelRateSelector<T> {
    pub fn new(timer_manager: T, update_interval: Duration, probe_sequence: ProbeSequence) -> Self {
        Self {
            timer_manager,
            update_interval,
            probe_sequence,
            peer_map: Default::default(),
            outdated_peers: Default::default(),
        }
    }

    pub fn add_peer(
        &mut self,
        assoc_cfg: &fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        let bssid: MacAddr = match assoc_cfg.bssid {
            None => {
                error!("Attempted to add peer with no BSSID.");
                return Err(zx::Status::INTERNAL);
            }
            Some(bssid) => bssid.into(),
        };
        if self.peer_map.contains_key(&bssid) {
            error!("Attempted to add peer {} twice.", &bssid);
        } else {
            let mut peer = Peer::from_assoc_cfg(assoc_cfg)?;
            if self.peer_map.is_empty() {
                self.timer_manager.schedule(self.update_interval);
            }
            peer.update_stats();
            self.peer_map.insert(bssid, peer);
        }
        Ok(())
    }

    pub fn remove_peer(&mut self, addr: &MacAddr) {
        self.outdated_peers.remove(addr);
        match self.peer_map.remove(addr) {
            Some(_) => debug!("Peer {} removed.", addr),
            None => debug!("Cannot remove peer {}, not found.", addr),
        }
        if self.peer_map.is_empty() {
            self.timer_manager.cancel();
        }
    }

    pub fn handle_tx_result_report(&mut self, tx_result: &fidl_common::WlanTxResult) {
        let peer_addr: MacAddr = tx_result.peer_addr.into();
        match self.peer_map.get_mut(&peer_addr) {
            Some(peer) => {
                peer.handle_tx_result_report(tx_result);
                self.outdated_peers.insert(peer_addr);
            }
            None => {
                debug!("Peer {} received tx status report after it was removed.", peer_addr);
            }
        }
    }

    fn update_stats(&mut self) {
        for outdated_peer in self.outdated_peers.drain() {
            self.peer_map.get_mut(&outdated_peer).map(|peer| peer.update_stats());
        }
    }

    pub fn handle_timeout(&mut self) {
        // Reschedule our timer so we keep updating in a loop.
        self.timer_manager.schedule(self.update_interval);
        self.update_stats();
    }

    pub fn get_tx_vector_idx(
        &mut self,
        frame_control: &FrameControl,
        peer_addr: &MacAddr,
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> Option<TxVecIdx> {
        match self.peer_map.get_mut(peer_addr) {
            None => TxVecIdx::new(ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 1),
            Some(peer) => {
                if frame_control.is_data() {
                    let needs_reliability =
                        flags.contains(fidl_softmac::WlanTxInfoFlags::FAVOR_RELIABILITY);
                    peer.get_tx_vector_idx(needs_reliability, &self.probe_sequence)
                } else {
                    peer.best_erp_for_reliability
                }
            }
        }
    }

    pub fn get_fidl_peers(&self) -> fidl_minstrel::Peers {
        fidl_minstrel::Peers {
            addrs: self.peer_map.iter().map(|(peer, _)| peer.to_array()).collect(),
        }
    }

    pub fn get_fidl_peer_stats(
        &self,
        peer_addr: &MacAddr,
    ) -> Result<fidl_minstrel::Peer, zx::Status> {
        let peer = self.peer_map.get(peer_addr).ok_or(zx::Status::NOT_FOUND)?;
        Ok(fidl_minstrel::Peer {
            addr: peer_addr.to_array(),
            max_tp: tx_vec_idx_opt_to_u16(&peer.best_expected_throughput),
            max_probability: tx_vec_idx_opt_to_u16(&peer.best_for_reliability),
            basic_highest: tx_vec_idx_opt_to_u16(&peer.highest_erp_rate),
            basic_max_probability: tx_vec_idx_opt_to_u16(&peer.best_erp_for_reliability),
            probes: peer.probes_total,
            entries: peer.tx_stats_map.iter().map(|(_, entry)| entry.into()).collect(),
        })
    }

    pub fn is_active(&self) -> bool {
        !self.peer_map.is_empty()
    }
}

fn tx_vec_idx_opt_to_u16(tx_vec_idx: &Option<TxVecIdx>) -> u16 {
    match tx_vec_idx {
        Some(idx) => **idx,
        None => 0,
    }
}

fn tx_time_ht(
    channel_bandwidth: fidl_common::ChannelBandwidth,
    gi: WlanGi,
    relative_mcs_idx: u8,
) -> Duration {
    header_tx_time_ht() + payload_tx_time_ht(channel_bandwidth, gi, relative_mcs_idx)
}

fn header_tx_time_ht() -> Duration {
    // TODO(https://fxbug.dev/42162502): Implement Plcp preamble and header
    Duration::ZERO
}

// relative_mcs_idx is the index for combination of (modulation, coding rate)
// tuple when listed in the same order as MCS Index, without nss. i.e. 0: BPSK,
// 1/2 1: QPSK, 1/2 2: QPSK, 3/4 3: 16-QAM, 1/2 4: 16-QAM, 3/4 5: 64-QAM, 2/3 6:
// 64-QAM, 3/4 7: 64-QAM, 5/6 8: 256-QAM, 3/4 (since VHT) 9: 256-QAM, 5/6 (since
// VHT)
fn payload_tx_time_ht(
    channel_bandwidth: fidl_common::ChannelBandwidth,
    gi: WlanGi,
    mcs_idx: u8,
) -> Duration {
    // N_{dbps} as defined in IEEE 802.11-2016 Table 19-26
    // Unit: Number of data bits per OFDM symbol (20 MHz channel width)
    const BITS_PER_SYMBOL_LIST: [u16; HT_NUM_UNIQUE_MCS as usize + /* since VHT */ 2] =
        [26, 52, 78, 104, 156, 208, 234, 260, /* since VHT */ 312, 347];
    // N_{sd} as defined in IEEE 802.11-2016 Table 19-26
    // Unit: Number of complex data numbers per spatial stream per OFDM symbol (20 MHz)
    const DATA_SUB_CARRIERS_20: u16 = 52;
    // Unit: Number of complex data numbers per spatial stream per OFDM symbol (40 MHz)
    const DATA_SUB_CARRIERS_40: u16 = 108;
    // TODO(https://fxbug.dev/42104244): VHT would have kDataSubCarriers80 = 234 and kDataSubCarriers160 = 468

    let nss = 1 + mcs_idx / HT_NUM_UNIQUE_MCS;
    let relative_mcs_idx = mcs_idx % HT_NUM_UNIQUE_MCS;
    let bits_per_symbol = if channel_bandwidth == fidl_common::ChannelBandwidth::Cbw40 {
        BITS_PER_SYMBOL_LIST[relative_mcs_idx as usize] * DATA_SUB_CARRIERS_40
            / DATA_SUB_CARRIERS_20
    } else {
        BITS_PER_SYMBOL_LIST[relative_mcs_idx as usize]
    };

    const TX_TIME_PER_SYMBOL_GI_800: Duration = Duration::from_nanos(4000);
    const TX_TIME_PER_SYMBOL_GI_400: Duration = Duration::from_nanos(3600);
    const TX_TIME_PADDING_GI_400: Duration = Duration::from_nanos(800);

    // Perform multiplication before division to prevent precision loss
    match gi {
        WlanGi::G_400NS => {
            TX_TIME_PADDING_GI_400
                + (TX_TIME_PER_SYMBOL_GI_400 * 8 * MINSTREL_FRAME_LENGTH)
                    / (nss as u32 * bits_per_symbol as u32)
        }
        WlanGi::G_800NS => {
            (TX_TIME_PER_SYMBOL_GI_800 * 8 * MINSTREL_FRAME_LENGTH)
                / (nss as u32 * bits_per_symbol as u32)
        }
        _ => panic!("payload_tx_time_ht is invalid for non-ht phy"),
    }
}

fn tx_time_erp(rate: &SupportedRate) -> Duration {
    header_tx_time_erp() + payload_tx_time_erp(rate)
}

fn header_tx_time_erp() -> Duration {
    // TODO(https://fxbug.dev/42162502): Implement Plcp preamble and header
    Duration::ZERO
}

fn payload_tx_time_erp(rate: &SupportedRate) -> Duration {
    // D_{bps} as defined in IEEE 802.11-2016 Table 17-4
    // Unit: Number of data bits per OFDM symbol
    let bits_per_symbol = rate.rate() * 2;
    const TX_TIME_PER_SYMBOL: Duration = Duration::from_nanos(4000);
    TX_TIME_PER_SYMBOL * 8 * MINSTREL_FRAME_LENGTH / bits_per_symbol as u32
}

fn erp_idx_stats(tx_vector_idx: TxVecIdx, rate: SupportedRate) -> TxStats {
    let perfect_tx_time = tx_time_erp(&rate);
    TxStats { perfect_tx_time, ..TxStats::new(tx_vector_idx) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_common as fidl_common;
    use lazy_static::lazy_static;
    use std::sync::{Arc, Mutex};
    use wlan_common::ie::{ChanWidthSet, HtCapabilityInfo};
    use wlan_common::mac::FrameType;
    use wlan_common::tx_vector::HT_START_IDX;

    struct MockTimerManager {
        scheduled: Arc<Mutex<Option<Duration>>>,
    }

    impl TimerManager for MockTimerManager {
        fn schedule(&mut self, from_now: Duration) {
            let mut scheduled = self.scheduled.lock().unwrap();
            scheduled.replace(from_now);
        }
        fn cancel(&mut self) {
            let mut scheduled = self.scheduled.lock().unwrap();
            scheduled.take();
        }
    }

    fn mock_minstrel() -> (MinstrelRateSelector<MockTimerManager>, Arc<Mutex<Option<Duration>>>) {
        let timer = Arc::new(Mutex::new(None));
        let timer_manager = MockTimerManager { scheduled: timer.clone() };
        let update_interval = Duration::from_micros(100);
        let probe_sequence = ProbeSequence::sequential();
        (MinstrelRateSelector::new(timer_manager, update_interval, probe_sequence), timer)
    }

    lazy_static! {
        static ref TEST_MAC_ADDR: MacAddr = MacAddr::from([50, 53, 51, 56, 55, 52]);
    }

    const BASIC_RATE_BIT: u8 = 0b10000000;

    fn ht_assoc_cfg() -> fidl_softmac::WlanAssociationConfig {
        let mut ht_cap = wlan_common::ie::fake_ht_capabilities();
        let mut ht_cap_info = HtCapabilityInfo(0);
        ht_cap_info.set_short_gi_40(true);
        ht_cap_info.set_short_gi_20(true);
        ht_cap_info.set_chan_width_set(ChanWidthSet::TWENTY_FORTY);
        ht_cap.ht_cap_info = ht_cap_info;
        ht_cap.mcs_set.0 = 0xffff; // Enable MCS 0-15

        fidl_softmac::WlanAssociationConfig {
            bssid: Some(TEST_MAC_ADDR.to_array()),
            aid: Some(42),
            listen_interval: Some(0),
            channel: Some(fidl_common::WlanChannel {
                primary: 149,
                cbw: fidl_common::ChannelBandwidth::Cbw40,
                secondary80: 0,
            }),
            qos: Some(true),
            wmm_params: None,
            rates: Some(vec![
                2,
                4,
                11,
                22,
                12 | BASIC_RATE_BIT,
                18,
                24,
                36,
                48,
                72,
                96,
                108 | BASIC_RATE_BIT,
            ]),
            capability_info: Some(0),
            ht_cap: Some(ht_cap.into()),
            ht_op: None,
            vht_cap: None,
            vht_op: None,
            ..Default::default()
        }
    }

    #[test]
    fn peer_from_assoc_cfg() {
        let assoc_cfg = ht_assoc_cfg();
        let peer = Peer::from_assoc_cfg(&assoc_cfg)
            .expect("Failed to convert WlanAssociationConfig into Peer.");
        assert_eq!(peer.addr, assoc_cfg.bssid.unwrap().into());
        assert_eq!(peer.tx_stats_map.len(), 24);
        let mut peer_rates = peer
            .tx_stats_map
            .keys()
            .into_iter()
            .map(|tx_vector_idx| **tx_vector_idx)
            .collect::<Vec<u16>>();
        peer_rates.sort();
        assert_eq!(
            peer_rates,
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, // HT rates
                129, 130, 131, 132, 133, 134, 135, 136, // ERP rates
            ]
        );
        let mut peer_erp_rates =
            peer.erp_rates.iter().map(|tx_vector_idx| **tx_vector_idx).collect::<Vec<u16>>();
        peer_erp_rates.sort();
        let expected_basic_rate_1 =
            TxVector::from_supported_rate(&SupportedRate(12 | BASIC_RATE_BIT)).unwrap().to_idx();
        let expected_basic_rate_2 =
            TxVector::from_supported_rate(&SupportedRate(108 | BASIC_RATE_BIT)).unwrap().to_idx();
        assert_eq!(peer_erp_rates, vec![*expected_basic_rate_1, *expected_basic_rate_2]);
        assert_eq!(peer.highest_erp_rate, TxVecIdx::new(136));
    }

    #[test]
    fn add_peer() {
        let (mut minstrel, timer) = mock_minstrel();
        assert!(timer.lock().unwrap().is_none()); // No timer is scheduled.
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        assert!(timer.lock().unwrap().is_some()); // A timer is scheduled.

        let peers = minstrel.get_fidl_peers();
        assert_eq!(peers.addrs.len(), 1);

        let peer_addr: MacAddr = {
            let mut peer_addr = [0u8; 6];
            peer_addr.copy_from_slice(&peers.addrs[0][..]);
            peer_addr.into()
        };
        let peer_stats =
            minstrel.get_fidl_peer_stats(&peer_addr).expect("Failed to get peer stats");
        assert_eq!(&peer_stats.addr, TEST_MAC_ADDR.as_array());
        // TODO(https://fxbug.dev/42103418): Size would be 40 if 40 MHz is supported and 72 if 40 MHz + SGI are supported.
        assert_eq!(peer_stats.entries.len(), 24);
        assert_eq!(peer_stats.max_tp, 16); // In the absence of data, our highest supported rate is max throughput.
        assert_eq!(peer_stats.basic_highest, ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 1);
        assert_eq!(peer_stats.basic_max_probability, ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 1);
    }

    #[test]
    fn remove_peer() {
        let (mut minstrel, timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        assert_eq!(minstrel.get_fidl_peers().addrs.len(), 1);
        assert!(timer.lock().unwrap().is_some()); // A timer is scheduled.

        minstrel.remove_peer(&TEST_MAC_ADDR);
        assert!(timer.lock().unwrap().is_none()); // No more peers -- timer cancelled.

        assert!(minstrel.get_fidl_peers().addrs.is_empty());
        assert_eq!(minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR), Err(zx::Status::NOT_FOUND));
    }

    #[test]
    fn remove_second_peer() {
        let (mut minstrel, timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        let mut peer2 = ht_assoc_cfg();
        peer2.bssid = Some([11, 12, 13, 14, 15, 16]);
        minstrel.add_peer(&peer2).expect("Failed to add peer.");
        assert_eq!(minstrel.get_fidl_peers().addrs.len(), 2);
        assert!(timer.lock().unwrap().is_some()); // A timer is scheduled.

        minstrel.remove_peer(&TEST_MAC_ADDR);
        assert_eq!(minstrel.get_fidl_peers().addrs.len(), 1);
        assert!(timer.lock().unwrap().is_some()); // A timer is still scheduled.

        assert_eq!(minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR), Err(zx::Status::NOT_FOUND));
        assert!(minstrel.get_fidl_peer_stats(&peer2.bssid.unwrap().into()).is_ok());
    }

    /// Helper fn to easily create tx result reports.
    fn make_tx_result(entries: Vec<(u16, u8)>, success: bool) -> fidl_common::WlanTxResult {
        assert!(entries.len() <= 8);
        let mut tx_result_entry =
            [fidl_common::WlanTxResultEntry { tx_vector_idx: 0, attempts: 0 }; 8];
        tx_result_entry[0..entries.len()].copy_from_slice(
            &entries
                .into_iter()
                .map(|(tx_vector_idx, attempts)| fidl_common::WlanTxResultEntry {
                    tx_vector_idx,
                    attempts,
                })
                .collect::<Vec<fidl_common::WlanTxResultEntry>>()[..],
        );
        let result_code = if success {
            fidl_common::WlanTxResultCode::Success
        } else {
            fidl_common::WlanTxResultCode::Failed
        };
        fidl_common::WlanTxResult {
            tx_result_entry,
            peer_addr: TEST_MAC_ADDR.to_array(),
            result_code,
        }
    }

    #[test]
    fn handle_tx_result_reports() {
        // Indicate that we failed to transmit on rates 16-14 and succeeded on 13.
        let tx_result = make_tx_result(vec![(16, 1), (15, 1), (14, 1), (13, 1)], true);

        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        minstrel.handle_tx_result_report(&tx_result);

        // Stats are not updated until after the timer fires.
        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        assert_eq!(peer_stats.max_tp, 16);

        minstrel.handle_timeout();
        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        assert_eq!(peer_stats.max_tp, 13);
        assert_eq!(peer_stats.max_probability, 13);

        let tx_result = make_tx_result(vec![(13, 1), (9, 1)], true);

        for _ in 0..10 {
            minstrel.handle_tx_result_report(&tx_result);
            minstrel.handle_timeout();
            let peer_stats =
                minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
            assert_eq!(peer_stats.max_probability, 9);
        }
        // We switch both max_probability and max_tp to rate 9 after enough observed failures on rate 13.
        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        assert_eq!(peer_stats.max_probability, 9);
        assert_eq!(peer_stats.max_tp, 9);
    }

    #[test]
    fn ht_rates_preferred() {
        let ht_tx_result_failed = make_tx_result(
            vec![
                (HT_START_IDX + 15, 1),
                (HT_START_IDX + 14, 1),
                (HT_START_IDX + 13, 1),
                (HT_START_IDX + 12, 1),
                (HT_START_IDX + 11, 1),
                (HT_START_IDX + 10, 1),
                (HT_START_IDX + 9, 1),
                (HT_START_IDX + 8, 1),
            ],
            false,
        );
        let ht_tx_result_success = make_tx_result(
            vec![
                (HT_START_IDX + 7, 1),
                (HT_START_IDX + 6, 1),
                (HT_START_IDX + 5, 1),
                (HT_START_IDX + 4, 1),
                (HT_START_IDX + 3, 1),
                (HT_START_IDX + 2, 1),
                (HT_START_IDX + 1, 1),
                // MCS 0 succeeds with success probability 11% (1/9).
                // This is greater than the cutoff of 1.0 - MINSTREL_PROBABILITY_THRESHOLD == 0.1.
                (HT_START_IDX + 0, 9),
            ],
            true,
        );
        // Highest ERP rate succeeds with 100% probability.
        let erp_tx_result_success =
            make_tx_result(vec![(ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 1, 1)], true);

        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        minstrel.handle_tx_result_report(&ht_tx_result_failed);
        minstrel.handle_tx_result_report(&ht_tx_result_success);
        minstrel.handle_tx_result_report(&erp_tx_result_success);
        minstrel.handle_timeout();

        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        // HT should be selected over ERP for max_tp and max_probability despite lower performance.
        assert_eq!(peer_stats.max_probability, HT_START_IDX);
        assert_eq!(peer_stats.max_tp, HT_START_IDX);
    }

    #[test]
    fn add_missing_rates() {
        let (mut minstrel, _timer) = mock_minstrel();
        let mut assoc_cfg = ht_assoc_cfg();
        // Remove top rates 96 and 108 from the supported list.
        let reduced_supported_rates = vec![2, 4, 11, 22, 12, 18, 24, 36, 48, 72];
        assoc_cfg.rates = Some(reduced_supported_rates);
        minstrel.add_peer(&assoc_cfg).expect("Failed to add peer.");

        let rate_108 = ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 1; // ERP, CBW20, GI 800 ns
        let rate_72 = ERP_START_IDX + ERP_NUM_TX_VECTOR as u16 - 3;

        // We should not have any stats for rate 108.
        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        assert!(!peer_stats.entries.iter().any(|entry| entry.tx_vector_idx == rate_108));

        // Fail transmission at unsupported rate 108, then succeed at 72.
        let tx_result = make_tx_result(vec![(rate_108, 1), (rate_72, 1)], true);
        minstrel.handle_tx_result_report(&tx_result);
        // Despite failure, we should now have stats for rate 108.
        let peer_stats =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats");
        assert!(peer_stats.entries.iter().any(|entry| entry.tx_vector_idx == rate_108));
    }

    #[track_caller]
    fn expect_probe_order(
        minstrel: &mut MinstrelRateSelector<MockTimerManager>,
        expected_probes: &[u16],
    ) {
        let mut probes_iter = expected_probes.iter();
        let max_tp =
            minstrel.get_fidl_peer_stats(&TEST_MAC_ADDR).expect("Failed to get peer stats").max_tp;
        let mut fc = FrameControl(0);
        fc.set_frame_type(FrameType::DATA);
        let flags = fidl_softmac::WlanTxInfoFlags::empty();

        for i in 0..(PROBE_INTERVAL as usize * expected_probes.len()) {
            let tx_vec_idx = minstrel.get_tx_vector_idx(&fc, &TEST_MAC_ADDR, flags);
            if i % PROBE_INTERVAL as usize == PROBE_INTERVAL as usize - 1 {
                // We expect a probe now.
                assert_eq!(*tx_vec_idx.unwrap(), *probes_iter.next().unwrap());
            } else {
                assert_eq!(*tx_vec_idx.unwrap(), max_tp);
            }
        }
    }

    #[test]
    fn expected_probe_order() {
        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");

        // We do not expect to probe rate 16 since it's max throughput,
        // or probe rate 136 since it's the highest basic rate.
        const EXPECTED_PROBES: [u16; 22] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, /* 16 ,*/
            129, 130, 131, 132, 133, 134, 135, /* 136 */
        ];

        expect_probe_order(&mut minstrel, &EXPECTED_PROBES[..]);
    }

    #[test]
    fn skip_seen_probes() {
        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        let tx_result = make_tx_result(vec![(16, 1), (15, 1), (14, 1)], true);
        minstrel.handle_tx_result_report(&tx_result);
        let tx_result = make_tx_result(vec![(13, 1)], true);
        minstrel.handle_tx_result_report(&tx_result);

        // We skip rates 13-16 since we've recently attempted tx on them.
        const UPDATED_PROBES: [u16; 19] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, /* 13, 14, 15, 16, */ 129, 130, 131, 132,
            133, 134, 135, /* 136 */
        ];

        expect_probe_order(&mut minstrel, &UPDATED_PROBES[..]);

        // Increment the probe cycle.
        minstrel.handle_timeout();

        // We skip 14 since it's now our max_tp and 15-16 since they're low probability.
        // We probe 13 since we haven't attempted tx in the last probe cycle.
        const UPDATED_PROBES_2: [u16; 20] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, /* 14, 15, 16, */ 129, 130, 131, 132,
            133, 134, 135, /* 136 */
        ];

        expect_probe_order(&mut minstrel, &UPDATED_PROBES_2[..]);
    }

    #[test]
    fn dead_probe_cycle_count() {
        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        let tx_result = make_tx_result(vec![(16, 1), (15, 1), (14, 1)], true);
        minstrel.handle_tx_result_report(&tx_result);
        minstrel.handle_timeout();

        // Probe rates 15 and 16 now have low probability and will not be probed.
        const EXPECTED_PROBES: [u16; 20] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, /* 14, 15, 16, */ 129, 130, 131, 132,
            133, 134, 135, /* 136 */
        ];
        expect_probe_order(&mut minstrel, &EXPECTED_PROBES[..]);

        for _ in 0..DEAD_PROBE_CYCLE_COUNT as usize {
            // Repeatedly increment probe_cycles_skipped for all rates.
            minstrel.outdated_peers.insert(*TEST_MAC_ADDR);
            minstrel.handle_timeout();
        }

        // We've passed enough cycles to probe rates 15 and 16 despite a 0% observed success rate.
        const EXPECT_DEAD_PROBES: [u16; 22] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, /* 14, */ 15, 16, 129, 130, 131, 132,
            133, 134, 135, /* 136 */
        ];
        expect_probe_order(&mut minstrel, &EXPECT_DEAD_PROBES[..]);
    }

    #[test]
    fn max_slow_probe() {
        let (mut minstrel, _timer) = mock_minstrel();
        minstrel.add_peer(&ht_assoc_cfg()).expect("Failed to add peer.");
        // Rate 16 is max_tp, max_probability, and 100% success rate.
        minstrel.handle_tx_result_report(&make_tx_result(vec![(16, 1)], true));
        minstrel.handle_timeout();

        const EXPECTED_PROBES: [u16; 22] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, /* 16, */ 129, 130, 131, 132,
            133, 134, 135, /* 136 */
        ];
        for _ in 0..MAX_SLOW_PROBE {
            // Probe rate 1 MAX_SLOW_PROBE times, incrementing num_probe_cycles_done as we go.
            expect_probe_order(&mut minstrel, &EXPECTED_PROBES[..]);
            minstrel.handle_tx_result_report(&make_tx_result(vec![(1, 1)], true));
        }

        // We should no longer probe rate 1 since it is slow compared to our selected rate 16.
        const NEW_EXPECTED_PROBES: [u16; 21] = [
            /*1, */ 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, /* 16, */ 129, 130,
            131, 132, 133, 134, 135, /* 136 */
        ];
        expect_probe_order(&mut minstrel, &NEW_EXPECTED_PROBES[..]);

        // After a timeout we probe rate 1 again.
        minstrel.handle_timeout();
        expect_probe_order(&mut minstrel, &EXPECTED_PROBES[..]);
    }

    #[track_caller]
    fn assert_data_rate(
        channel_bandwidth: fidl_common::ChannelBandwidth,
        gi: WlanGi,
        relative_mcs_idx: u8,
        expected_mbit_per_second: f64,
    ) {
        let tx_time = tx_time_ht(channel_bandwidth, gi, relative_mcs_idx);
        const BYTES_PER_MBIT: f64 = 125000.0;
        let mut expected_tx_time =
            (MINSTREL_FRAME_LENGTH as f64 / BYTES_PER_MBIT) / expected_mbit_per_second;
        if gi == WlanGi::G_400NS {
            // Add 800ns test interval for short gap tx times. This becomes significant at high data rates.
            expected_tx_time += Duration::from_nanos(800).as_secs_f64();
        }
        let actual_tx_time = tx_time.as_secs_f64();
        let ratio = expected_tx_time / actual_tx_time;
        assert!(ratio < 1.01 && ratio > 0.99);
    }

    #[test]
    fn tx_time_ht_approx_values_cbw20() {
        // IEEE 802.11-2016 Tables 19-27 through 19-30 list data rates for CBW20. We test a sample here.
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_800NS, 0, 6.5);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_400NS, 0, 7.2);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_800NS, 8, 13.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_400NS, 8, 14.4);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_800NS, 31, 260.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw20, WlanGi::G_400NS, 31, 288.9);
    }

    #[test]
    fn tx_time_ht_approx_values_cbw40() {
        // IEEE 802.11-2016 Tables 19-32 through 19-34 list data rates for CBW40. We test a sample here.
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_800NS, 0, 13.5);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_400NS, 0, 15.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_800NS, 8, 27.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_400NS, 8, 30.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_800NS, 31, 540.0);
        assert_data_rate(fidl_common::ChannelBandwidth::Cbw40, WlanGi::G_400NS, 31, 600.0);
    }
}
