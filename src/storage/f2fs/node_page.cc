// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {
void NodePage::FillNodeFooter(nid_t nid, nid_t ino, uint32_t ofs) {
  NodeFooter &raw_footer = node().footer;
  raw_footer.nid = CpuToLe(nid);
  raw_footer.ino = CpuToLe(ino);
  raw_footer.flag = CpuToLe(ofs << static_cast<int>(BitShift::kOffsetBitShift));
}

void NodePage::CopyNodeFooterFrom(NodePage &src) {
  memcpy(&node().footer, &src.node().footer, sizeof(NodeFooter));
}

void NodePage::FillNodeFooterBlkaddr(block_t blkaddr) {
  Checkpoint &ckpt = fs()->GetSuperblockInfo().GetCheckpoint();
  NodeFooter &raw_footer = node().footer;
  raw_footer.cp_ver = CpuToLe(ckpt.checkpoint_ver);
  raw_footer.next_blkaddr = CpuToLe(blkaddr);
}

nid_t NodePage::InoOfNode() const { return LeToCpu(node().footer.ino); }

nid_t NodePage::NidOfNode() const { return LeToCpu(node().footer.nid); }

uint32_t NodePage::OfsOfNode() const {
  uint32_t flag = LeToCpu(node().footer.flag);
  return flag >> static_cast<int>(BitShift::kOffsetBitShift);
}

uint64_t NodePage::CpverOfNode() const { return LeToCpu(node().footer.cp_ver); }

block_t NodePage::NextBlkaddrOfNode() const { return LeToCpu(node().footer.next_blkaddr); }

// f2fs assigns the following node offsets described as (num).
// N = kNidsPerBlock
//
//  Inode block (0)
//    |- direct node (1)
//    |- direct node (2)
//    |- indirect node (3)
//    |            `- direct node (4 => 4 + N - 1)
//    |- indirect node (4 + N)
//    |            `- direct node (5 + N => 5 + 2N - 1)
//    `- double indirect node (5 + 2N)
//                 `- indirect node (6 + 2N)
//                       `- direct node (x(N + 1))
bool NodePage::IsDnode() const {
  uint32_t ofs = OfsOfNode();
  if (ofs == kOfsIndirectNode1 || ofs == kOfsIndirectNode2 || ofs == kOfsDoubleIndirectNode) {
    return false;
  }
  if (ofs >= kOfsDoubleIndirectNode + 1) {
    ofs -= kOfsDoubleIndirectNode + 1;
    if (ofs % (kNidsPerBlock + 1)) {
      return false;
    }
  }
  return true;
}

void NodePage::SetNid(size_t off, nid_t nid) {
  WaitOnWriteback();

  if (IsInode()) {
    node().i.i_nid[off - kNodeDir1Block] = CpuToLe(nid);
  } else {
    node().in.nid[off] = CpuToLe(nid);
  }
}

nid_t NodePage::GetNid(size_t off) const {
  if (IsInode()) {
    return LeToCpu(node().i.i_nid[off - kNodeDir1Block]);
  }
  return LeToCpu(node().in.nid[off]);
}

bool NodePage::IsColdNode() const {
  uint32_t flag = LeToCpu(node().footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kColdBitShift)).ValueOrDie();
  return flag & bit;
}

bool NodePage::IsFsyncDnode() const {
  uint32_t flag = LeToCpu(node().footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kFsyncBitShift)).ValueOrDie();
  return flag & bit;
}

bool NodePage::IsDentDnode() const {
  uint32_t flag = LeToCpu(node().footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kDentBitShift)).ValueOrDie();
  return flag & bit;
}

void NodePage::SetColdNode(const bool is_dir) {
  Node &raw_node = node();
  uint32_t flag = LeToCpu(raw_node.footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kColdBitShift)).ValueOrDie();
  if (is_dir) {
    flag &= ~bit;
  } else {
    flag |= bit;
  }
  raw_node.footer.flag = CpuToLe(flag);
}

void NodePage::SetFsyncMark(bool mark) {
  Node &raw_node = node();
  uint32_t flag = LeToCpu(raw_node.footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kFsyncBitShift)).ValueOrDie();
  if (mark) {
    flag |= bit;
  } else {
    flag &= ~bit;
  }
  raw_node.footer.flag = CpuToLe(flag);
}

void NodePage::SetDentryMark(bool mark) {
  Node &raw_node = node();
  uint32_t flag = LeToCpu(raw_node.footer.flag);
  uint32_t bit =
      safemath::CheckLsh(1U, static_cast<uint32_t>(BitShift::kDentBitShift)).ValueOrDie();
  if (mark) {
    flag |= bit;
  } else {
    flag &= ~bit;
  }
  raw_node.footer.flag = CpuToLe(flag);
}

uint32_t NodePage::StartBidxOfNode(const uint32_t num_addrs) const {
  uint32_t node_ofs = OfsOfNode(), NumOfIndirectNodes = 0;

  if (node_ofs == kOfsInode) {
    return 0;
  } else if (node_ofs <= kOfsDirectNode2) {
    NumOfIndirectNodes = 0;
  } else if (node_ofs >= kOfsIndirectNode1 && node_ofs < kOfsIndirectNode2) {
    NumOfIndirectNodes = 1;
  } else if (node_ofs >= kOfsIndirectNode2 && node_ofs < kOfsDoubleIndirectNode) {
    NumOfIndirectNodes = 2;
  } else {
    NumOfIndirectNodes = (node_ofs - kOfsDoubleIndirectNode - 2) / (kNidsPerBlock + 1);
  }

  uint32_t bidx = node_ofs - NumOfIndirectNodes - 1;
  return (num_addrs + safemath::CheckMul(bidx, kAddrsPerBlock)).ValueOrDie();
}

bool NodePage::IsInode() const {
  NodeFooter &raw_footer = node().footer;
  return raw_footer.nid == raw_footer.ino;
}

block_t *NodePage::addrs_array() const {
  Node &raw_node = node();
  if (IsInode()) {
    Inode &inode = raw_node.i;
    if (inode.i_inline & kExtraAttr) {
      return inode.i_addr + (inode.i_extra_isize / sizeof(uint32_t));
    }
    return inode.i_addr;
  }
  return raw_node.dn.addr;
}

block_t NodePage::GetBlockAddr(const size_t offset) const { return LeToCpu(addrs_array()[offset]); }

void NodePage::SetBlockAddr(const size_t offset, const block_t new_addr) const {
  addrs_array()[offset] = CpuToLe(new_addr);
}

}  // namespace f2fs
