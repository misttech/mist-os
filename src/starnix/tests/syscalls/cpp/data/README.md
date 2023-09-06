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
