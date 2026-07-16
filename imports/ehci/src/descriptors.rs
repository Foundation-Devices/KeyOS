//! Standard USB descriptor types, parsing and helpers.

extern crate alloc;

use alloc::vec::Vec;
use core::{mem::size_of, ops::Deref};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    error::{EhciError, Result},
    EndpointDirection,
};

/// A container that allows parsing a byte sequence of Device + Config descriptors
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
pub struct DescriptorSet {
    bytes: Vec<u8>,
}

/// Standard USB descriptor types.
#[allow(missing_docs)]
pub enum DescriptorType {
    Device = 0x01,
    Config = 0x02,
    Interface = 0x04,
    Endpoint = 0x05,
}

/// Standard USB descriptor header common to all descriptor types.
#[derive(Clone, Debug, Default, Immutable, KnownLayout, FromBytes, IntoBytes)]
#[repr(C, packed)]
#[allow(missing_docs)]
pub struct DescriptorHeader {
    pub length: u8,
    pub descriptor_type: u8,
}

/// Standard USB device descriptor as defined in USB 2.0 chapter 9,
/// not including the standard header.
#[derive(Copy, Clone, Debug, Default, Immutable, KnownLayout, FromBytes, IntoBytes)]
#[repr(C, packed)]
#[allow(missing_docs)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub device_class: u8,
    pub device_sub_class: u8,
    pub device_protocol: u8,
    pub max_packet_size: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device: u16,
    pub manufacturer_string: u8,
    pub product_string: u8,
    pub serial_number: u8,
    pub num_configurations: u8,
}

/// Standard USB configuration descriptor as defined in USB 2.0 chapter 9,
/// not including the standard header.
#[derive(Copy, Clone, Debug, Default, Immutable, KnownLayout, FromBytes, IntoBytes)]
#[repr(C, packed)]
#[allow(missing_docs)]
pub struct ConfigDescriptor {
    pub total_length: u16,
    pub num_interfaces: u8,
    pub configuration_value: u8,
    pub configuration: u8,
    pub attributes: u8,
    pub max_power: u8,
}

/// Standard USB interface descriptor as defined in USB 2.0 chapter 9,
/// not including the standard header.
#[derive(Copy, Clone, Debug, Default, Immutable, KnownLayout, FromBytes, IntoBytes)]
#[repr(C, packed)]
#[allow(missing_docs)]
pub struct InterfaceDescriptor {
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub num_endpoints: u8,
    pub interface_class: u8,
    pub interface_sub_class: u8,
    pub interface_protocol: u8,
    pub interface_string: u8,
}

/// Standard USB endpoint descriptor as defined in USB 2.0 chapter 9,
/// not including the standard header.
#[derive(Copy, Clone, Debug, Default, Immutable, KnownLayout, FromBytes, IntoBytes)]
#[repr(C, packed)]
#[allow(missing_docs)]
pub struct EndpointDescriptor {
    pub endpoint_address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

const ENDPOINT_DESCRIPTOR_DIRECTION_MASK: u8 = 1 << 7;
const ENDPOINT_DESCRIPTOR_NUMBER_MASK: u8 = 0xf;
const ENDPOINT_DESCRIPTOR_ATTRIBUTES_TYPE_MASK: u8 = 0x3;

/// Endpoint types.
#[derive(PartialEq, Eq)]
#[allow(missing_docs)]
pub enum EndpointType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl EndpointDescriptor {
    /// Get direction of this endpoint.
    pub fn get_direction(&self) -> EndpointDirection {
        let direction = self.endpoint_address & ENDPOINT_DESCRIPTOR_DIRECTION_MASK;
        if direction != 0 {
            EndpointDirection::In
        } else {
            EndpointDirection::Out
        }
    }

    /// Get endpoint number.
    pub fn get_endpoint_number(&self) -> u8 { self.endpoint_address & ENDPOINT_DESCRIPTOR_NUMBER_MASK }

    /// Get endpoint type.
    pub fn get_endpoint_type(&self) -> Option<EndpointType> {
        let ep_type = self.attributes & ENDPOINT_DESCRIPTOR_ATTRIBUTES_TYPE_MASK;
        match ep_type {
            0 => Some(EndpointType::Control),
            1 => Some(EndpointType::Isochronous),
            2 => Some(EndpointType::Bulk),
            3 => Some(EndpointType::Interrupt),
            _ => None,
        }
    }
}

impl DescriptorSet {
    /// Try to create a descriptor set object from the given bytes.
    /// Bytes must start with a valid Device Descriptor.
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let header = DescriptorHeader::read_from_prefix(&bytes).map_err(|_| EhciError::DescriptorError)?.0;
        if header.descriptor_type != DescriptorType::Device as u8
            || header.length as usize > bytes.len()
            || (header.length as usize) < size_of::<DescriptorHeader>() + size_of::<DeviceDescriptor>()
        {
            Err(EhciError::DescriptorError)
        } else {
            Ok(Self { bytes })
        }
    }

    /// Get Device Descriptor
    pub fn device(&self) -> DeviceDescriptor {
        DeviceDescriptor::read_from_prefix(&self.bytes[2..]).unwrap().0
    }

    /// Iterate over configurations
    pub fn configurations(&self) -> ConfigIterator<'_> {
        ConfigIterator { max_num: self.device().num_configurations, offset: 0, ds: self }
    }

    /// Get the original bytes of the descriptor set
    pub fn bytes(&self) -> &[u8] { &self.bytes }
}

macro_rules! descriptor_impl {
    ($Type:ident, $Iterator:ident, $Entry:ident, $DT:ident) => {
        /// Iterates over descriptors
        #[derive(Clone)]
        pub struct $Iterator<'a> {
            offset: usize,
            ds: &'a DescriptorSet,
            max_num: u8,
        }

        /// Wrapper struct that can deref to the descriptor or continue
        /// getting child descriptors, if applicable.
        #[derive(Clone)]
        pub struct $Entry<'a> {
            #[allow(dead_code)]
            length: u8,
            offset: usize,
            ds: &'a DescriptorSet,
        }

        impl<'a> Iterator for $Iterator<'a> {
            type Item = $Entry<'a>;

            fn next(&mut self) -> Option<Self::Item> {
                if self.max_num == 0 {
                    return None;
                }
                loop {
                    let header =
                        DescriptorHeader::read_from_prefix(self.ds.bytes.get(self.offset..)?).ok()?.0;
                    if (header.length as usize) < size_of::<DescriptorHeader>() {
                        return None;
                    }
                    let data_offset = self.offset;
                    self.offset += header.length as usize;
                    if self.offset > self.ds.bytes.len() {
                        return None;
                    }
                    if header.descriptor_type == DescriptorType::$DT as u8
                        && header.length as usize >= size_of::<$Type>() + 2
                    {
                        self.max_num -= 1;
                        return Some($Entry { length: header.length, offset: data_offset, ds: self.ds });
                    }
                }
            }
        }

        impl<'a> Deref for $Entry<'a> {
            type Target = $Type;

            fn deref(&self) -> &Self::Target {
                $Type::ref_from_prefix(&self.ds.bytes[(self.offset + 2)..]).unwrap().0
            }
        }

        impl<'a> core::fmt::Debug for $Iterator<'a> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_list().entries(self.clone()).finish()
            }
        }
    };
}

descriptor_impl!(ConfigDescriptor, ConfigIterator, ConfigDescriptorEntry, Config);
descriptor_impl!(InterfaceDescriptor, InterfaceIterator, InterfaceDescriptorEntry, Interface);
descriptor_impl!(EndpointDescriptor, EndpointIterator, EndpointDescriptorEntry, Endpoint);

impl<'a> ConfigDescriptorEntry<'a> {
    /// Iterate over the interfaces of the configuration.
    pub fn interfaces(&self) -> InterfaceIterator<'_> {
        InterfaceIterator {
            offset: self.offset + self.length as usize,
            ds: self.ds,
            max_num: self.num_interfaces,
        }
    }
}

impl<'a> InterfaceDescriptorEntry<'a> {
    /// Iterate over the endpoints of the interface
    pub fn endpoints(&self) -> EndpointIterator<'_> {
        EndpointIterator {
            offset: self.offset + self.length as usize,
            ds: self.ds,
            max_num: self.num_endpoints,
        }
    }
}

impl core::fmt::Debug for DescriptorSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DescriptorSet")
            .field("Device", &self.device())
            .field("Configs", &self.configurations())
            .finish()
    }
}

impl<'a> core::fmt::Debug for ConfigDescriptorEntry<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Config")
            .field("configuration", self.deref())
            .field("interfaces", &self.interfaces())
            .finish()
    }
}
impl<'a> core::fmt::Debug for InterfaceDescriptorEntry<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Interface")
            .field("interface", self.deref())
            .field("endpoints", &self.endpoints())
            .finish()
    }
}
impl<'a> core::fmt::Debug for EndpointDescriptorEntry<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { self.deref().fmt(f) }
}
