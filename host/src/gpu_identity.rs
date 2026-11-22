#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PciAddress {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl PciAddress {
    fn new(segment: u16, bus: u8, device: u8, function: u8) -> Option<Self> {
        (device < 32 && function < 8).then_some(Self {
            segment,
            bus,
            device,
            function,
        })
    }

    /// CUDA reports the segment, bus, device and function in hexadecimal.
    pub(crate) fn from_cuda_bus_id(id: &str) -> Option<Self> {
        let (segment, rest) = id.split_once(':')?;
        let (bus, rest) = rest.split_once(':')?;
        let (device, function) = rest.split_once('.')?;
        Self::new(
            u16::from_str_radix(segment, 16).ok()?,
            u8::from_str_radix(bus, 16).ok()?,
            u8::from_str_radix(device, 16).ok()?,
            u8::from_str_radix(function, 16).ok()?,
        )
    }

    /// Windows reports the bus separately and packs the device number in
    /// the high word of the address and the function number in the low word.
    pub(crate) fn from_windows_properties(bus: u32, address: u32) -> Option<Self> {
        Self::new(
            0,
            bus.try_into().ok()?,
            (address >> 16).try_into().ok()?,
            (address & 0xffff).try_into().ok()?,
        )
    }
}

/// The newest compute capability among the matched adapters. The Windows
/// properties do not identify the PCI segment, so a nonzero CUDA domain
/// leaves the match uncertain. CUDA can also see cards in TCC mode that
/// DXGI does not list, and NVENC may be able to use them.
pub(crate) fn matched_compute_capability(
    dxgi: &[PciAddress],
    cuda: &[(PciAddress, Option<i32>)],
) -> Option<i32> {
    if cuda.iter().any(|(address, major)| {
        address.segment != 0 || (!dxgi.contains(address) && !matches!(major, Some(1..=3)))
    }) {
        return None;
    }
    let mut newest = None;
    for (index, address) in dxgi.iter().enumerate() {
        if dxgi[..index].contains(address) {
            return None;
        }
        let mut matches = cuda.iter().filter(|(id, _)| id == address);
        let (_, major) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        newest = newest.max(Some(major.filter(|&major| major > 0)?));
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::NvidiaDriverUpdate::{self, Supported, Unknown, Unsupported};

    fn verdict(dxgi: &[PciAddress], cuda: &[(PciAddress, Option<i32>)]) -> NvidiaDriverUpdate {
        NvidiaDriverUpdate::from_compute_capability(matched_compute_capability(dxgi, cuda))
    }

    #[test]
    fn equal_device_counts_with_different_pci_addresses_leave_the_card_unknown() {
        let display = PciAddress::from_windows_properties(10, 0).unwrap();
        let cuda = PciAddress::from_cuda_bus_id("0000:0b:00.0").unwrap();
        assert_eq!(verdict(&[display], &[(cuda, Some(3))]), Unknown);
    }

    #[test]
    fn the_windows_bus_and_packed_address_match_the_cuda_pci_id() {
        let display = PciAddress::from_windows_properties(10, (31 << 16) | 7).unwrap();
        assert_eq!(Some(display), PciAddress::from_cuda_bus_id("0000:0a:1f.7"));
        for (bus, address) in [
            (256, 0),
            (10, 32 << 16),
            (10, 8),
            (10, 0x100),
            (10, u32::MAX),
        ] {
            assert_eq!(PciAddress::from_windows_properties(bus, address), None);
        }
    }

    #[test]
    fn a_nonzero_cuda_domain_leaves_even_a_matching_bus_and_address_unknown() {
        let display = PciAddress::from_windows_properties(10, 0).unwrap();
        let cuda = PciAddress::from_cuda_bus_id("0000:0a:00.0").unwrap();
        let other_domain = PciAddress::from_cuda_bus_id("0001:0a:00.0").unwrap();
        assert_eq!(
            verdict(&[display], &[(cuda, Some(3)), (other_domain, Some(3))]),
            Unknown
        );
    }

    #[test]
    fn every_dxgi_card_gets_its_verdict_from_the_matching_cuda_device() {
        let kepler = PciAddress::from_windows_properties(10, 0).unwrap();
        let maxwell = PciAddress::from_windows_properties(10, 1 << 16).unwrap();
        let cuda_kepler = PciAddress::from_cuda_bus_id("0000:0a:00.0").unwrap();
        let cuda_maxwell = PciAddress::from_cuda_bus_id("0000:0a:01.0").unwrap();
        let compute_only = PciAddress::from_cuda_bus_id("0000:0b:00.0").unwrap();
        for major in [Some(8), None, Some(0), Some(4)] {
            let mixed = [(compute_only, major), (cuda_kepler, Some(3))];
            assert_eq!(verdict(&[kepler], &mixed), Unknown);
            assert_eq!(verdict(&[kepler, maxwell], &mixed), Unknown);
        }
        assert_eq!(
            verdict(
                &[kepler],
                &[(compute_only, Some(3)), (cuda_kepler, Some(3))]
            ),
            Unsupported
        );
        let both = [(cuda_maxwell, Some(5)), (cuda_kepler, Some(3))];
        assert_eq!(verdict(&[kepler, maxwell], &both), Supported);
        assert_eq!(
            verdict(&[kepler], &[(cuda_kepler, Some(3)), (cuda_kepler, Some(3))]),
            Unknown
        );
    }
}
