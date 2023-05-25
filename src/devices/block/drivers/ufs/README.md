# Universal Flash Storage (UFS)
Universal Flash Storage (UFS) is a flash-based mobile storage device that provides high
performance, low power, and high reliability compared to eMMC. Unlike eMMC, which uses a parallel
bus, UFS uses a high-speed, multi-lane serial bus to improve performance. The serial interface of
UFS uses LVDS differential signaling, which enables high-speed signal transmission with high noise
immunity and low voltage.
UFS follows the UFS specification of the JEDEC standard, which is currently up to version 4.0.
Fuchsia's UFS driver is based on the UFS specification version 3.1.

## Specification
UFS: https://www.jedec.org/document_search?search_api_views_fulltext=jesd220f
UFSHCI: https://www.jedec.org/standards-documents/docs/jesd223e

## Layered Architecture
The layered architecture of UFS uses the M-PHY physical layer and the UniPro link layer, with
the UFS Transfer Protocol (UTP) transport layer on top. It also adopts a SCSI architecture model
and command protocol that supports multiple simultaneous commands and command queues on top of the
UTP transport layer. The layered architecture of UFS consists of three layers: UFS Application
Layer(UAP), UFS Transport Protocol Layer (UTP), and UFS InterConnect Layer (UIC). The UFS driver is
implemented according to this layered architecture.

### UFS Application Layer (UAP)
The UAP layer consists of the UFS Command Set Layer (UCS), Device Manager, and Task Manager.
- The UFS Command Set Layer processes SCSI commands.
- Device Manager is responsible for device settings such as data transfer and power mode control,
  and controls them through descriptor, attribute, and flag requests.
- Task Manager is responsible for processing the task management function for command queue control.

### UFS Transport Protocol Layer (UTP)
UTP layer uses UFS Protocol Information Unit (UPIU) for message delivery between UAP layer and UFS
Interconnect (UIC) layer. The UTP layer wraps information from the UAP layer in a UPIU and delivers
it to the UFS device.

### UFS InterConnect Layer (UIC)
The lowest layer, the UIC layer, handles the connection between the UFS host interface and the UFS
device. The UIC layer automatically detects and recovers I/O errors through the UniPro link layer,
and provides reliable data transfer through the M-PHY physical layer.

## TODO
* Refactoring I/O queue
* Support large size I/O
* Support FLUSH command
* Support FUA flag
* Support TRIM command (unmap)
* Support write booster
* Support Power Mode
* Support RPMB logical unit
* Support well-known LU
* Support Utp Task Management Request
* Implement Ufs tools
* Integration tests
* Support high speed gear 2
* Support UFS 4.0 Spec (multi queue)
* Support multiple platform (exynos, snapdragon, etc.)

## How to test

### Ufs driver unit tests (not yet ready)
* Build configuration for ufs tests
> $ fx set core.x64 --with //src/devices/block/drivers/ufs:tests

* Run ufs unit tests
> $ fx test ufs-unit-tests

### Test on real device
Currently, the only device that supports a PCI-based UFS host controller is the Samsung Galaxy Book
S(NT767XCL) with an Intel Lakefield CPU(i5-L16G7). The Galaxy Book S has 512GB of eUFS 3.0 storage.
Unfortunately, we can't run Fuchsia OS on that laptop yet, but we'll find a way.
