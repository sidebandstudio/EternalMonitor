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

    /// Windows reports decimal numbers and omits the segment on a
    /// single-segment system. Unrecognized location strings are unknown.
    pub(crate) fn from_windows_location(location: &str) -> Option<Self> {
        let location = location.strip_prefix("PCI ")?;
        let (segment, location) = match location.strip_prefix("segment ") {
            Some(location) => {
                let (segment, location) = location.split_once(' ')?;
                (segment.parse().ok()?, location)
            }
            None => (0, location),
        };
        let (bus, location) = location.strip_prefix("bus ")?.split_once(", device ")?;
        let (device, function) = location.split_once(", function ")?;
        Self::new(
            segment,
            bus.parse().ok()?,
            device.parse().ok()?,
            function.parse().ok()?,
        )
    }
}

/// The newest compute capability among the DXGI adapters, once every
/// adapter has its own confirmed CUDA match. CUDA can also see cards
/// that are only available for compute.
pub(crate) fn matched_compute_capability(
    dxgi: &[PciAddress],
    cuda: &[(PciAddress, Option<i32>)],
) -> Option<i32> {
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

    #[test]
    fn equal_device_counts_with_different_pci_addresses_leave_the_card_unknown() {
        let display =
            PciAddress::from_windows_location("PCI bus 10, device 0, function 0").unwrap();
        let compute_only = PciAddress::from_cuda_bus_id("0000:0b:00.0").unwrap();
        let other_segment = PciAddress::from_cuda_bus_id("0001:0a:00.0").unwrap();
        for wrong_device in [compute_only, other_segment] {
            assert_eq!(
                NvidiaDriverUpdate::from_compute_capability(matched_compute_capability(
                    &[display],
                    &[(wrong_device, Some(3))],
                )),
                Unknown
            );
        }
    }

    #[test]
    fn every_dxgi_card_gets_its_verdict_from_the_matching_cuda_device() {
        let kepler = PciAddress::from_windows_location("PCI bus 10, device 0, function 0").unwrap();
        let maxwell =
            PciAddress::from_windows_location("PCI segment 2 bus 10, device 1, function 0")
                .unwrap();
        let cuda_kepler = PciAddress::from_cuda_bus_id("0000:0a:00.0").unwrap();
        let cuda_maxwell = PciAddress::from_cuda_bus_id("0002:0a:01.0").unwrap();
        let compute_only = PciAddress::from_cuda_bus_id("0000:0b:00.0").unwrap();
        let verdict = |dxgi: &[PciAddress], cuda: &[(PciAddress, Option<i32>)]| {
            NvidiaDriverUpdate::from_compute_capability(matched_compute_capability(dxgi, cuda))
        };
        let kepler_only = [(compute_only, Some(8)), (cuda_kepler, Some(3))];
        assert_eq!(verdict(&[kepler], &kepler_only), Unsupported);
        assert_eq!(verdict(&[kepler, maxwell], &kepler_only), Unknown);
        let both = [(cuda_maxwell, Some(5)), (cuda_kepler, Some(3))];
        assert_eq!(verdict(&[kepler, maxwell], &both), Supported);
        assert_eq!(
            verdict(&[kepler], &[(cuda_kepler, Some(3)), (cuda_kepler, Some(3))]),
            Unknown
        );
    }
}
