# syscalls test data

## ext4 image

Created without the 64bit feature with:

* `truncate -s 1M simple_ext4.img`
* `mkfs.ext4 simple_ext4.img -O ^64bit`
* `sudo mkdir /mnt/tmp`
* `sudo mount -oloop simple_ext4.img /mnt/tmp`
* `sudo cp hello_world.txt /mnt/tmp/`
* `sudo umount /mnt/tmp`
* `e2fsck -f simple_ext4.img`
* `resize2fs -M simple_ext4.img` (counting number of reported blocks)
* `truncate -o --size NN simple_ext4.img` (where NN=number of blocks above)


## hash tree file

The DM_TABLE_LOAD ioctl for dm-verity requires the user to pass in a hashtree file that contains
a merkle tree that is generated from the contents of the ext4 image. The following commands
allow us to make use of the losetup and veritysetup command line tools to both generate the
hashtree file.

1. Set up two loop devices. One backing the ext4 image and one backing the hashtree file.
* `sudo losetup -f src/starnix/tests/syscalls/rust/data/simple_ext4.img`
* `sudo dd if=/dev/zero bs=4k conv=notrunc oflag=append count=2 of=/tmp/hashtree`

NOTE: count here is the number of blocks that we need to store the merkle tree. Can be
adjusted for larger ext4 images.
* `sudo losetup -f /tmp/hashtree`
2. Figure out which loop devices are associated with which files.
* `sudo losetup -a`

    e.g.

    `/dev/loop19: (/usr/local/google/home/nikitajindal/fuchsia/src/starnix/tests/syscalls/rust/data/simple_ext4.img)`

    `/dev/loop20: (/tmp/hashtree)`

3. Use `veritysetup format` to generate the merkle tree and populate the hashtree file with it. Note that `veritysetup format`
takes several optional arguments. Here we just set the salt. The format command will print out the root hash of the merkle tree. The root hash is copied into data/root_hash.txt.
* `sudo veritysetup format /dev/loop19 /dev/loop20 --salt ffffffffffffffff`
4. Copy the contents of /tmp/hashtree into data/hashtree_truncated.txt, removing the first block.
* `dd if=/tmp/hashtree of=src/starnix/tests/syscalls/rust/data/hashtree_truncated.txt bs=1 skip=4096`
