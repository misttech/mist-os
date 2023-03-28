// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_NODE_PAGE_H_
#define SRC_STORAGE_F2FS_NODE_PAGE_H_

#include <fbl/recycler.h>

#include "src/storage/f2fs/file_cache.h"

namespace f2fs {

class NodePage : public Page, public fbl::Recyclable<NodePage> {
 public:
  NodePage() = delete;
  NodePage(FileCache *file_cache, pgoff_t index) : Page(file_cache, index) {}
  NodePage(const NodePage &) = delete;
  NodePage &operator=(const NodePage &) = delete;
  NodePage(const NodePage &&) = delete;
  NodePage &operator=(const NodePage &&) = delete;
  void fbl_recycle() { RecyclePage(); }

  void FillNodeFooter(nid_t nid, nid_t ino, uint32_t ofs);
  void CopyNodeFooterFrom(NodePage &src);
  void FillNodeFooterBlkaddr(block_t blkaddr);
  nid_t InoOfNode() const;
  nid_t NidOfNode() const;
  uint32_t OfsOfNode() const;
  uint64_t CpverOfNode() const;
  block_t NextBlkaddrOfNode() const;
  bool IsDnode() const;
  bool IsColdNode() const;
  bool IsFsyncDnode() const;
  bool IsDentDnode() const;
  void SetColdNode(const bool is_dir);
  void SetFsyncMark(const bool mark);
  void SetDentryMark(const bool mark);

  bool IsInode() const;
  block_t GetBlockAddr(const size_t offset) const;
  void SetBlockAddr(const size_t offset, const block_t addr) const;

  // It returns the starting file offset that |node_page| indicates.
  // The file offset can be calcuated by using the node offset that |node_page| has.
  // See NodePage::IsDnode().
  uint32_t StartBidxOfNode(const uint32_t num_addrs) const;

  void SetNid(size_t off, nid_t nid);
  nid_t GetNid(size_t off) const;

 private:
  Node &node() const { return *GetAddress<Node>(); }
  block_t *addrs_array() const;

  static constexpr uint32_t kOfsInode = 0;
  static constexpr uint32_t kOfsDirectNode1 = 1;
  static constexpr uint32_t kOfsDirectNode2 = 2;
  static constexpr uint32_t kOfsIndirectNode1 = 3;
  static constexpr uint32_t kOfsIndirectNode2 = 4 + kNidsPerBlock;
  static constexpr uint32_t kOfsDoubleIndirectNode = 5 + 2 * kNidsPerBlock;
};
}  // namespace f2fs
#endif  // SRC_STORAGE_F2FS_NODE_PAGE_H_
