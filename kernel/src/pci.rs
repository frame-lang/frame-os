// kernel/src/pci.rs
//
// Minimal PCI configuration-space access (B4 Step 1). Pure native. Uses the
// legacy CAM port pair: write a 32-bit address to 0xCF8, read/write the dword
// at 0xCFC. Enough to discover a device by vendor/device id, read its BARs +
// interrupt line, and enable I/O + bus-mastering — which is all the legacy
// virtio-blk driver needs.
//
// Config address layout (0xCF8):
//   bit 31      enable
//   bits 23:16  bus
//   bits 15:11  device
//   bits 10:8   function
//   bits 7:2    register (dword-aligned offset)

use crate::io;

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// A located PCI device (bus/device/function), with helpers to read its
/// configuration space.
#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

impl PciDevice {
    /// Read a 32-bit dword from this device's config space at `offset` (which
    /// is rounded down to a dword boundary).
    pub fn read_u32(&self, offset: u8) -> u32 {
        io::outl(
            CONFIG_ADDR,
            address(self.bus, self.device, self.function, offset),
        );
        io::inl(CONFIG_DATA)
    }

    /// Write a 32-bit dword to this device's config space at `offset`.
    pub fn write_u32(&self, offset: u8, val: u32) {
        io::outl(
            CONFIG_ADDR,
            address(self.bus, self.device, self.function, offset),
        );
        io::outl(CONFIG_DATA, val);
    }

    /// Base I/O port of BAR `n` (BARs are at 0x10, 0x14, ...). Assumes an I/O
    /// BAR (bit 0 set); returns the port base with the low flag bits masked.
    pub fn bar_io(&self, n: u8) -> u16 {
        let bar = self.read_u32(0x10 + n * 4);
        (bar & 0xFFFF_FFFC) as u16
    }

    /// The interrupt line (legacy PIC IRQ number) from config offset 0x3C.
    pub fn interrupt_line(&self) -> u8 {
        (self.read_u32(0x3C) & 0xFF) as u8
    }

    /// Enable I/O-space decoding (bit 0) and bus-mastering/DMA (bit 2) in the
    /// command register (offset 0x04).
    pub fn enable_io_and_bus_master(&self) {
        let cmd = self.read_u32(0x04);
        self.write_u32(0x04, cmd | 0b101);
    }
}

/// Scan bus 0 for the first device matching `vendor`/`device` ids.
/// (QEMU puts virtio devices on bus 0; a recursive multi-bus scan isn't needed.)
pub fn find(vendor: u16, device: u16) -> Option<PciDevice> {
    for dev in 0..32u8 {
        for func in 0..8u8 {
            let d = PciDevice {
                bus: 0,
                device: dev,
                function: func,
            };
            let id = d.read_u32(0x00);
            let (ven, did) = ((id & 0xFFFF) as u16, (id >> 16) as u16);
            if ven == 0xFFFF {
                continue; // no device/function here
            }
            if ven == vendor && did == device {
                return Some(d);
            }
        }
    }
    None
}
