block-verity
============

The block-verity driver supports integrity validation of factory partitions. It builds verification
data and verifies that the data has not changed by performing block-level merkle verification
against a root hash.