use lpc11uxx_rom::{RomDriver, iap};
use lpc11uxx_rom::usbd::{HidInitParameter, InitParameter, CoreDescriptors,
    SetupPacket, HidReport, DeviceDescriptor, UsbHandle, HidHandle};
use lpc11uxx::{Peripherals, CorePeripherals, Interrupt};
use crate::lpc11uxx_misc::*;
use cortex_m::peripheral::scb::SystemHandler;
use cortex_m::peripheral::NVIC;
use heapless::spsc::Queue;
use static_assertions::const_assert_eq;
use core::convert::TryInto;

use crate::MAIN_CLOCK_FREQ;

use core::ptr::NonNull;
use core::slice;

pub static mut USBD_HANDLE: UsbHandle = UsbHandle::null();
static mut TIMER_ENABLED: bool = false;
static mut TIMER_ELAPSED: bool = false;
static mut SHOULD_REINVOKE_ISP: bool = false;
static mut USART_STATE: u8 = 0;
static mut CUR_LED_BLINK_TICK: u8 = 0;
static mut USART_PACKET: [u8; 0x10] = [0; 0x10];
static mut SHOULD_SEND_USART_PACKET: bool = false;
static mut HID_REPORT_PACKET: [u8; 0x40] = [0; 0x40];
static mut FLASH_BUFFER_LEN: usize = 0;
static mut FLASH_BUFFER: [u8; 512] = [0; 512];
static mut FLASH_CUR_IDX: usize = 0;

// TODO: Generate the descriptors with const fns.
static USB_HID_REPORT_DATA_DESC: &[u8] = &[
    0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x15,
    0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
    0x09, 0x01, 0x81, 0x02, 0x95, 0x40, 0x09, 0x01,
    0x91, 0x02, 0x95, 0x40, 0x09, 0x01, 0xb1, 0x02,
    0xc0
];

static DEVICE_DESCRIPTOR: DeviceDescriptor = DeviceDescriptor {
    length: 18,
    descriptor_type: 1,
    bcd_usb: 0x200,
    device_class: 0xEF,
    device_sub_class: 0x02,
    device_protocol: 0x01,
    max_packet_size: 0x40,
    id_vendor: 0x28de,
    id_product: 0x1002,
    bcd_device: 0x100,
    manufacturer_str_index: 1,
    product_str_index: 2,
    serial_number_str_index: 0,
    num_configurations: 1,
};

static STRING_DESCRIPTOR: &[u8] = &[
    // Language descriptor
    0x04, 0x03, 0x09, 0x04,
    // "Valve Software" descriptor
    0x1e, 0x03, 0x56, 0x00, 0x61, 0x00, 0x6c, 0x00,
    0x76, 0x00, 0x65, 0x00, 0x20, 0x00, 0x53, 0x00,
    0x6f, 0x00, 0x66, 0x00, 0x74, 0x00, 0x77, 0x00,
    0x61, 0x00, 0x72, 0x00, 0x65, 0x00,
    // "Wired Controller Bootloader" descriptor
    0x38, 0x03, 0x57, 0x00, 0x69, 0x00, 0x72, 0x00,
    0x65, 0x00, 0x64, 0x00, 0x20, 0x00, 0x43, 0x00,
    0x6f, 0x00, 0x6e, 0x00, 0x74, 0x00, 0x72, 0x00,
    0x6f, 0x00, 0x6c, 0x00, 0x6c, 0x00, 0x65, 0x00,
    0x72, 0x00, 0x20, 0x00, 0x42, 0x00, 0x6f, 0x00,
    0x6f, 0x00, 0x74, 0x00, 0x6c, 0x00, 0x6f, 0x00,
    0x61, 0x00, 0x64, 0x00, 0x65, 0x00, 0x72, 0x00,
    // "Serial" descriptor
    0x10, 0x03, 0x53, 0x00, 0x65, 0x00, 0x72, 0x00,
    0x69, 0x00, 0x61, 0x00, 0x6c, 0x00,

    // Terminator
    // Fun fact: NXP's examples all have an OOB Read vulnerability on their
    // String Descriptor. They don't null-terminate the array. It's probably
    // a bit hard to weaponize, but still, wtf NXP.
    0x00
];

use crate::usb_descriptors::*;
use crate::combine_descriptors;

const HID_INTERFACE: u8 = 0x00;
const HID_ENDPOINT: u8 = 0x81;
const CDC_CIF_INTERFACE: u8 = 0x01;
const CDC_DIF_INTERFACE: u8 = 0x02;
const USB_CDC_IN_EP: u8 = 0x82;
const USB_CDC_OUT_EP: u8 = 0x03;
const USB_CDC_INT_EP: u8 = 0x83;


const CONFIGURATION_DESCRIPTOR_CONST: [u8; 101] = combine_descriptors![
    ConfigurationDescriptor {
        total_length: 100,
        num_interfaces: 3,
        //total_length: 34,
        //num_interfaces: 1,
        configuration_value: 1,
        configuration_name_idx: 0,
        attributes: ConfigurationAttributes::BUS_POWERED,
        max_power: 0x32,
    },
    InterfaceDescriptor {
        interface_num: HID_INTERFACE,
        alternate_setting: 0,
        num_endpoints: 1,
        interface_class: 3,
        interface_subclass: 0,
        interface_protocol: 0,
        interface_name_idx: 0
    },
    HidDescriptor {
        hid_version: 0x01_11,
        country_code: 0,
        hid_descriptors_num: 1,
    },
    HidDescriptorListItem {
        descriptor_type: 0x22,
        descriptor_length: 33
    },
    EndpointDescriptor {
        endpoint_addr: 0x81,
        attributes: 0x03,
        max_packet_size: 64,
        interval: 6
    },

    InterfaceAssociationDescriptor {
        first_interface: CDC_CIF_INTERFACE,
        interface_count: 2,
        function_class: 0x02,
        subfunction_class: 0x02,
        function_protocol: 0x00,
        function_name_idx: 0x04,
    },

    InterfaceDescriptor {
        interface_num: CDC_CIF_INTERFACE,
        alternate_setting: 0,
        num_endpoints: 1,
        interface_class: 0x02,
        interface_subclass: 0x02,
        interface_protocol: 0,
        interface_name_idx: 0x04,
    },

    CdcHeaderFunctionalDescriptor {
        cdc_version: 01_10,
    },

    CdcCallManagementFunctionalDescriptor {
        capabilities: 0x01,
        data_interface: CDC_DIF_INTERFACE,
    },

    CdcAbstractControlManagementFunctionalDescriptor {
        capabilities: 0x02
    },

    CdcUnionFunctionalDescriptor {
        master_interface: CDC_CIF_INTERFACE,
        num_slave_interfaces: 1,
    },

    CdcUnionSlaveInterface {
        interface_num: CDC_DIF_INTERFACE
    },

    EndpointDescriptor {
        endpoint_addr: USB_CDC_INT_EP,
        attributes: 0x03,
        max_packet_size: 0x10,
        interval: 2
    },

    InterfaceDescriptor {
        interface_num: CDC_DIF_INTERFACE,
        alternate_setting: 0,
        num_endpoints: 2,
        interface_class: 0x0A,
        interface_subclass: 0x00,
        interface_protocol: 0,
        interface_name_idx: 0x04,
    },

    EndpointDescriptor {
        endpoint_addr: USB_CDC_IN_EP,
        attributes: 0x02,
        max_packet_size: 0x40,
        interval: 0
    },

    EndpointDescriptor {
        endpoint_addr: USB_CDC_OUT_EP,
        attributes: 0x02,
        max_packet_size: 0x40,
        interval: 0
    }
];

const fn find_iface_pos(iface_num: u8) -> Option<usize> {
    let mut idx = 0;
    while idx < CONFIGURATION_DESCRIPTOR_CONST.len() {
        let len = CONFIGURATION_DESCRIPTOR_CONST[idx];
        if len == 0 {
            return None
        }

        if CONFIGURATION_DESCRIPTOR_CONST[idx + 1] == 4 && CONFIGURATION_DESCRIPTOR_CONST[idx + 2] == iface_num {
            return Some(idx);
        }

        idx += len as usize;
    }

    return None
}

static mut CONFIGURATION_DESCRIPTOR: [u8; CONFIGURATION_DESCRIPTOR_CONST.len()] = CONFIGURATION_DESCRIPTOR_CONST;

static LED_BLINK_INTENSITY_LOOP: &[u16] = &[
      0x0,     0x1,     0x1,     0x1,
      0x1,     0x1,     0x2,     0x3,
      0x4,     0x5,     0x7,     0x9,
      0xB,     0xE,    0x11,    0x14,
     0x18,    0x1C,    0x20,    0x24,
     0x29,    0x2E,    0x34,    0x3A,
     0x40,    0x47,    0x4F,    0x56,
     0x5F,    0x67,    0x70,    0x79,
     0x83,    0x8E,    0x99,    0xA5,
     0xB1,    0xBD,    0xCA,    0xD7,
     0xE5,    0xF4,   0x103,   0x112,
    0x123,   0x133,   0x144,   0x156,
    0x169,   0x17C,   0x18F,   0x1A3,
    0x1B8,   0x1CD,   0x1E3,   0x1FA,
    0x211,   0x229,   0x242,   0x25B,
    0x274,   0x28F,   0x2AA,   0x2C6,
    0x2E2,   0x2FF,   0x31D,   0x33B,
    0x35B,   0x37A,   0x39B,   0x3BC,
    0x3DE,   0x400,   0x424,   0x448,
    0x46D,   0x492,   0x4B8,   0x4DF,
    0x507,   0x530,   0x559,   0x583,
    0x5AE,   0x5D9,   0x605,   0x632,
    0x660,   0x68F,   0x6BF,   0x6EF,
    0x720,   0x751,   0x784,   0x7B8,
    0x7EC,   0x821,   0x857,   0x88E,
    0x8C5,   0x8FE,   0x937,   0x971,
    0x9AC,   0x9E8,   0xA24,   0xA62,
    0xAA0,   0xAE0,   0xB20,   0xB61,
    0xBA2,   0xBE5,   0xC29,   0xC6E,
    0xCB3,   0xCF9,   0xD41,   0xD89,
    0xDD2,   0xE1C,   0xE67,   0xEB3,
    0xF00,   0xF4D,   0xF9C,   0xFEC,
];

extern fn hid_get_report_handler(_handle: HidHandle, setup_packet: *const SetupPacket, buffer: *mut *mut u8, length: *mut u16) -> i32 {
    match unsafe { (*setup_packet).value.high() } {
        1 | 2 => return 0x40002,
        3 => {
            let buffer = unsafe {
                // TOTALLY SAAAAAFE. I sure hope so at least.
                core::slice::from_raw_parts_mut(*buffer, 0x40)
            };
            buffer.copy_from_slice(unsafe { &HID_REPORT_PACKET });
            unsafe { *length = 0x40 };
        },
        _ => (),
    }
    0
}

fn write_data_to_program2_flash(data: &[u8]) -> i32 {
    unsafe {
        let mut buffer_cap = FLASH_BUFFER.len() - FLASH_BUFFER_LEN;

        if data.len() <= buffer_cap {
            buffer_cap = data.len();
        }

        FLASH_BUFFER[FLASH_BUFFER_LEN..FLASH_BUFFER_LEN + buffer_cap].copy_from_slice(&data[..buffer_cap]);

        if buffer_cap < data.len() {
            if FLASH_CUR_IDX == 0 {
                // Put -1 in Reserved3 of vector table, to prevent accidentally
                // booting a partially flashed
                FLASH_BUFFER[9 * 4..10 * 4].copy_from_slice(&(-1_i32).to_le_bytes());
            }
            // We start from 0x2000, since that's where program2 starts.
            let flash_dst = FLASH_CUR_IDX + 0x2000;

            if flash_dst + FLASH_BUFFER.len() >= 0x20_000 {
                return 1;
            }

            let err = iap::prepare_sector_for_write(2, 0x1f);
            if err != 0 {
                return 1;
            }
            let err = iap::copy_ram_to_flash(flash_dst as u32, FLASH_BUFFER.as_ptr() as usize, FLASH_BUFFER.len(), unsafe { MAIN_CLOCK_FREQ } / 1024);
            if err != 0 {
                return 1;
            }
            FLASH_CUR_IDX += 0x200;
            FLASH_BUFFER[..data.len() - buffer_cap].copy_from_slice(&data[buffer_cap..]);
            FLASH_BUFFER_LEN = data.len() - buffer_cap;
        } else {
            FLASH_BUFFER_LEN += buffer_cap;
        }
    }
    0
}

fn check_fmc_signature(expected_sig: &[u8]) -> bool {
    let peripherals = unsafe { Peripherals::steal() };

    peripherals.FLASHCTRL.fmsstart.write(|v| unsafe { v.start().bits(0x2030 / 16) });
    peripherals.FLASHCTRL.fmstatclr.write(|v| v.sig_done_clr().set_bit());
    let mut flash_stop = unsafe { (FLASH_CUR_IDX + 0x2000) / 16 };
    if unsafe { FLASH_CUR_IDX } % 16 == 0 {
        flash_stop -= 1;
    }
    peripherals.FLASHCTRL.fmsstop.write(|v| unsafe { v
        .stop().bits(flash_stop as u32)
        .sig_start().set_bit()
    });

    while peripherals.FLASHCTRL.fmstat.read().sig_done().bit_is_clear() {}

    let mut sig = [0u8; 0x10];
    sig[0x0..0x04].copy_from_slice(&peripherals.FLASHCTRL.fmsw0.read().bits().to_le_bytes());
    sig[0x4..0x08].copy_from_slice(&peripherals.FLASHCTRL.fmsw1.read().bits().to_le_bytes());
    sig[0x8..0x0c].copy_from_slice(&peripherals.FLASHCTRL.fmsw2.read().bits().to_le_bytes());
    sig[0xc..0x10].copy_from_slice(&peripherals.FLASHCTRL.fmsw3.read().bits().to_le_bytes());

    return sig == expected_sig;
}

fn end_flash_verify_firmware_sig(sig: &[u8]) -> u32 {
    unsafe {
        if FLASH_BUFFER_LEN != 0 {
            for elem in &mut FLASH_BUFFER[FLASH_BUFFER_LEN..] {
                *elem = 0xff;
            }
            let flash_dst = FLASH_CUR_IDX + 0x2000;

            if flash_dst + FLASH_BUFFER.len() >= 0x20_000 {
                return 1;
            }

            let err = iap::prepare_sector_for_write(2, 0x1f);
            if err != 0 {
                return 2;
            }
            let err = iap::copy_ram_to_flash(flash_dst as u32, FLASH_BUFFER.as_ptr() as usize, FLASH_BUFFER.len(), unsafe { MAIN_CLOCK_FREQ } / 1024);
            if err != 0 {
                return 3;
            }
            FLASH_CUR_IDX += FLASH_BUFFER_LEN;
        }
    }

    if !check_fmc_signature(sig) {
        return 4;
    }

    // If the signatures match, the flash was successful. Let's put the
    // magic value in the Reserved3 slot of the Vector Table to allow
    // booting.
    let mut program2_vector_table_copy = [0u8; 4096];
    let program2_vector_table = unsafe { slice::from_raw_parts(0x2_000 as *const u8, program2_vector_table_copy.len()) };
    program2_vector_table_copy.copy_from_slice(program2_vector_table);
    program2_vector_table_copy[9 * 4..10 * 4].copy_from_slice(&0xecaabac0_u32.to_le_bytes());

    let err = iap::prepare_sector_for_write(2,2);
    if err != 0 {
        return 5;
    }

    let err = iap::erase_sectors(2, 2, unsafe { MAIN_CLOCK_FREQ } / 1024);
    if err != 0 {
        return 6;
    }

    let err = iap::prepare_sector_for_write(2,2);
    if err != 0 {
        return 7;
    }

    let err = iap::copy_ram_to_flash(0x2_000, program2_vector_table_copy.as_ptr() as usize, program2_vector_table_copy.len(), unsafe { MAIN_CLOCK_FREQ } / 1024);
    if err != 0 {
        return 8;
    }
    return 0;
}

fn write_report_0x94(err_code: u16) {
    unsafe {
        HID_REPORT_PACKET = [0; 0x40];
        HID_REPORT_PACKET[0] = 0x94;
        HID_REPORT_PACKET[1] = 2;
        HID_REPORT_PACKET[2..4].copy_from_slice(&err_code.to_le_bytes());
    }
}

fn hid_handle_set_feature_report(buffer: &[u8]) {
    match buffer[0] {
        // GET_HWINFO
        0x83 => {
            crate::usb_debug_uart::usb_putb(b"GET_HWINFO\n");
            unsafe {
                HID_REPORT_PACKET = [0; 0x40];
                HID_REPORT_PACKET[0] = 0x83;
                HID_REPORT_PACKET[1] = 0xf;
                HID_REPORT_PACKET[2] = 1;
                HID_REPORT_PACKET[3..7].copy_from_slice(&0x1002_u32.to_le_bytes());
                HID_REPORT_PACKET[7] = 4;
                // Get version from the Vector Table
                let bootloader_version = *(0 as *const u32).offset(9);
                HID_REPORT_PACKET[8..12].copy_from_slice(&bootloader_version.to_le_bytes());
                HID_REPORT_PACKET[12] = 9;
                HID_REPORT_PACKET[13..17].copy_from_slice(&super::EEPROM_CACHE.version.to_le_bytes());
            }
            return;
        },
        0x90 => {
            crate::usb_debug_uart::usb_putb(b"REINVOKE_ISP\n");
            unsafe { SHOULD_REINVOKE_ISP = true; }
            return;
        },
        0x91 => {
            crate::usb_debug_uart::usb_putb(b"ERASE_PROGRAM2\n");
            write_report_0x94(2);
            unsafe {
                FLASH_CUR_IDX = 0;
                FLASH_BUFFER_LEN = 0;
            }
            let err = iap::prepare_sector_for_write(2, 0x1f);
            if err != 0 {
                write_report_0x94(1);
                return;
            }
            let err = iap::erase_sectors(2, 0x1f, unsafe { MAIN_CLOCK_FREQ } / 1024);
            if err != 0 {
                write_report_0x94(1);
                return;
            }
            write_report_0x94(0);
            return;
        },
        0x92 => {
            // Spams a bit too much :D
            //crate::usb_debug_uart::usb_putb(b"FLASH_FIRMWARE\n");

            let err = write_data_to_program2_flash(&buffer[2..2 + usize::from(buffer[1])]);
            let err = 0;
            write_report_0x94(err as u16);
            led_advance_blink();
            unsafe {
                HID_REPORT_PACKET = [0; 0x40];
                HID_REPORT_PACKET[0] = 0x92;
            }
            return;
        },
        0x93 => {
            crate::usb_debug_uart::usb_putb(b"VERIFY_FIRMWARE_SIG\n");
            let err = end_flash_verify_firmware_sig(&buffer[2..2 + 0x10]);
            write_report_0x94(err as u16);
            //let peripherals = unsafe { Peripherals::steal() };
            //crate::usb_debug_uart::usb_putnbr_hex(peripherals.FLASHCTRL.fmsw0.read().bits());
            //crate::usb_debug_uart::usb_putb(b" ");
            //crate::usb_debug_uart::usb_putnbr_hex(peripherals.FLASHCTRL.fmsw1.read().bits());
            //crate::usb_debug_uart::usb_putb(b" ");
            //crate::usb_debug_uart::usb_putnbr_hex(peripherals.FLASHCTRL.fmsw2.read().bits());
            //crate::usb_debug_uart::usb_putb(b" ");
            //crate::usb_debug_uart::usb_putnbr_hex(peripherals.FLASHCTRL.fmsw3.read().bits());
            //crate::usb_debug_uart::usb_putb(b"\n");
            return;
        },
        0x95 => {
            crate::usb_debug_uart::usb_putb(b"RESET_WHOLE_SOC\n");
            if buffer[1] == 0 {
                usart_send_reset();
                super::setup_watchdog(10_000);
            }
            return;
        },
        0x97 => {
            crate::usb_debug_uart::usb_putb(b"NRF_ERASE_PROGRAM\n");
            usart_send_text_transmission(b"Y");
            write_report_0x94(2);
            return;
        },
        0x98 => {
            // crate::usb_debug_uart::usb_putb(b"NRF_FLASH_PROGRAM\n");
            usart_send_z_packet(&buffer[2..2 + usize::from(buffer[1])]);
            write_report_0x94(2);
            return;
        },
        0x99 => {
            crate::usb_debug_uart::usb_putb(b"NRF_VERIFY_FIRMWARE_SIG\n");
            usart_send_sig_packet(&buffer[2..2 + 0x10]);
            write_report_0x94(2);
            return;
        },
        0xa0 => {
            crate::usb_debug_uart::usb_putb(b"SET_HARDWARE_VERSION\n");
            if buffer[1] == 4 {
                let version = u32::from_le_bytes(buffer[2..6].try_into().unwrap());
                unsafe { super::EEPROM_CACHE.version = version; }
                super::write_eeprom_cache();
            }
            return;
        },
        n => {
            crate::usb_debug_uart::usb_putnbr_hex(n as u32);
            crate::usb_debug_uart::usb_putb(b"\n");
            return;
        }
    }
}

extern fn hid_set_report_handler(_handle: HidHandle, setup_packet: *const SetupPacket, buffer: *const *const u8, length: u16) -> i32 {
    if length != 0 {
        match unsafe { (*setup_packet).value.high() } {
            1 | 2 => return 0x40002,
            3 => {
                let buffer = unsafe {
                    core::slice::from_raw_parts(*buffer, length as usize)
                };
                hid_handle_set_feature_report(buffer);
            },
            _ => ()
        }
    }
    0
}

fn init_usb_hid(handle: UsbHandle, hid_interface_descriptor: &mut [u8], mem_base: &mut u32, mem_size: &mut u32) -> i32 {
    let mut hid_param = HidInitParameter::default();
    hid_param.max_reports = 1;
    static mut USB_HID_REPORT_DATA: HidReport = HidReport {
        len: 0x21,
        idle_time: 0,
        pad: 0,
        desc: Some(unsafe { NonNull::new_unchecked(USB_HID_REPORT_DATA_DESC.as_ptr() as *mut u8).cast() })
    };
    // TODO:
    /*if hid_interface_descriptor.interface_class != 3 {
        return -1;
    }*/
    hid_param.report_data = Some(unsafe { &USB_HID_REPORT_DATA });
    hid_param.get_report = Some(hid_get_report_handler);
    hid_param.set_report = Some(hid_set_report_handler);
    hid_param.mem_base = *mem_base;
    hid_param.mem_size = *mem_size;
    hid_param.intf_desc = Some(NonNull::from(hid_interface_descriptor).cast());
    let err = (RomDriver::get().usb_api().hid().init)(handle, &mut hid_param);
    if err != 0 {
        return err
    }
    *mem_base = hid_param.mem_base;
    *mem_size = hid_param.mem_size;

    return 0;
}

fn init_usb() -> i32 {
    let mut core_peripherals = unsafe { CorePeripherals::steal() };
    let peripherals = unsafe { Peripherals::steal() };

    // Set USB clock source to PLL OUT
    peripherals.SYSCON.usbclksel.write(|v| v.sel().usb_pll_out());
    peripherals.SYSCON.usbclkuen.write(|v| v.ena().no_change());
    peripherals.SYSCON.usbclkuen.write(|v| v.ena().update_clock_source());
    peripherals.SYSCON.usbclkdiv.write(|v| unsafe { v.div().bits(1) });

    // Enable USB and USBRAM clocks.
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.usb().enabled());
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.usbram().enabled());

    // Initialize USB handle
    let usb_api = RomDriver::get().usb_api();

    let mut init_param = InitParameter::default();
    init_param.usb_reg_base = 0x40080000;
    init_param.max_num_ep = 5;
    init_param.mem_base = 0x20004000;
    init_param.mem_size = 0x800;

    let mut desc = CoreDescriptors::default();
    desc.device_descriptors = Some(NonNull::from(&DEVICE_DESCRIPTOR));
    desc.string_descriptors = Some(NonNull::from(STRING_DESCRIPTOR).cast());
    desc.high_speed_descriptors = Some(NonNull::from(unsafe { &mut CONFIGURATION_DESCRIPTOR }).cast());
    desc.full_speed_descriptors = Some(NonNull::from(unsafe { &mut CONFIGURATION_DESCRIPTOR }).cast());

    let err = (usb_api.hw().init)(unsafe { &mut USBD_HANDLE }, &desc, &mut init_param);

    if err != 0 {
        return err;
    }

    init_param.mem_base = 0x20004800 - init_param.mem_size;

    // Initialize USB HID
    const HID_INTERFACE_OFFSET: usize = match find_iface_pos(HID_INTERFACE) {
        Some(val) => val,
        None => panic!("HID_INTERFACE not found")
    };
    const _: () = if CONFIGURATION_DESCRIPTOR_CONST[HID_INTERFACE_OFFSET + 5] != 3 {
        panic!("INVALID HID DESCRIPTOR");
    };
    let err = init_usb_hid(unsafe { USBD_HANDLE }, unsafe { &mut CONFIGURATION_DESCRIPTOR[HID_INTERFACE_OFFSET..HID_INTERFACE_OFFSET + 9] }, &mut init_param.mem_base, &mut init_param.mem_size);
    if err != 0 {
        return err;
    }

    const CDC_CIF_INTERFACE_OFFSET: usize = match find_iface_pos(CDC_CIF_INTERFACE) {
        Some(val) => val,
        None => panic!("CDC_CIF_INTERFACE not found")
    };
    const CDC_DIF_INTERFACE_OFFSET: usize = match find_iface_pos(CDC_DIF_INTERFACE) {
        Some(val) => val,
        None => panic!("CDC_DIF_INTERFACE not found")
    };
    let err = crate::usb_debug_uart::init_usb_cdc(unsafe { USBD_HANDLE },
        unsafe { &mut CONFIGURATION_DESCRIPTOR[CDC_CIF_INTERFACE_OFFSET..CDC_CIF_INTERFACE_OFFSET + 9] },
        unsafe { &mut CONFIGURATION_DESCRIPTOR[CDC_DIF_INTERFACE_OFFSET..CDC_DIF_INTERFACE_OFFSET + 9] },
        &mut init_param.mem_base, &mut init_param.mem_size, USB_CDC_IN_EP as u32, USB_CDC_OUT_EP as u32
    );
    if err != 0 {
        return err;
    }

    // Enable USB_IRQ and set priority to 1.
    unsafe {
        core_peripherals.NVIC.set_priority(Interrupt::USB_IRQ, 0);
    }
    core_peripherals.NVIC.enable(Interrupt::USB_IRQ);
    unsafe {
        core_peripherals.NVIC.set_priority(Interrupt::USB_IRQ, 1);
    }

    // Connect to host!
    (usb_api.hw().connect)(unsafe { USBD_HANDLE }, 1);

    return 0;
}

fn init_usart() {
    let mut core_peripherals = unsafe { CorePeripherals::steal() };
    let peripherals = unsafe { Peripherals::steal() };

    unsafe {
        USART_STATE = 0;
    }

    // RingBuffer_Init is unnecessary - we're using ArrayDeque

    // Chip_UART_Init
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.usart().enabled());
    peripherals.SYSCON.uartclkdiv.write(|v| unsafe { v.div().bits(1) });
    peripherals.USART.fcr_mut().write(|v| v
        .fifoen().enabled()
        .rxfifores().clear()
        .txfifores().clear());
    peripherals.USART.lcr.write(|v| v
        .wls()._8_bit_character_leng()
        .sbs()._1_stop_bit());
    peripherals.USART.fdr.write(|v| unsafe { v.mulval().bits(1) });

    // ChipUART_SetupFifos
    peripherals.USART.fcr_mut().write(|v| v
        .fifoen().enabled()
        .rxtl().level2());

    // Enable access to the divisor registers.
    peripherals.USART.lcr.modify(|_, v| v.dlab().enable_access_to_div());

    // Set the USART divisor latch to 3
    peripherals.USART.dll().write(|v| unsafe { v.dllsb().bits(3) });

    peripherals.USART.fdr.write(|v| unsafe {
        v
            .divaddval().bits(1)
            .mulval().bits(11)
    });

    // Disable access to the divisor registers, restore access to USART read/write registers.
    peripherals.USART.lcr.modify(|_, v| v.dlab().disable_access_to_di());

    unsafe { NVIC::unmask(Interrupt::USART) };
    peripherals.USART.ier_mut().modify(|_, v| v
        .rbrinten().enable_the_rda_inter()
        .rlsinten().enable_the_rls_inter());

    // TODO: Make sure those priority numbers are correct. NVIC_SetPriority does
    // fancy bit shifting I don't fully understand at 3AM.
    unsafe { core_peripherals.NVIC.set_priority(Interrupt::USART, 0) };
    unsafe { core_peripherals.SCB.set_priority(SystemHandler::PendSV, 1) };
}

static mut USART_RING_BUFFER: Queue<u8, heapless::consts::U256> = Queue(heapless::i::Queue::new());

fn RingBuffer_InsertMult(ring_buffer: &mut Queue<u8, heapless::consts::U256>, data: &[u8]) -> usize {
    let old_len = ring_buffer.len();
    // TODO: Please tell me this turns into a simple memcpy...
    for item in data {
        if ring_buffer.enqueue(*item).is_err() {
            break;
        }
    }
    ring_buffer.len() - old_len
}

fn usart_send_raw_str(data: &[u8]) -> usize {
    let peripherals = unsafe { Peripherals::steal() };

    // First, disable send interrupts.
    peripherals.USART.ier_mut().modify(|_, v| v.threinten().disable_the_thre_int());

    // Insert data to the ring buffer
    let mut inserted_len = RingBuffer_InsertMult(unsafe { &mut USART_RING_BUFFER }, data);

    // Send the contents of the ring buffer
    while peripherals.USART.lsr.read().thre().is_empty() {
        if let Some(val) = unsafe { USART_RING_BUFFER .dequeue() } {
            peripherals.USART.thr_mut().write(|v| unsafe { v.thr().bits(val) });
        } else {
            break;
        }
    }

    // Try to insert some more contents in the ring buffer.
    inserted_len += RingBuffer_InsertMult(unsafe { &mut USART_RING_BUFFER }, &data[inserted_len..]);

    // Re-enable send interrupts
    peripherals.USART.ier_mut().modify(|_, v| v.threinten().enable_the_thre_inte());

    inserted_len
}

fn usart_send_02() {
    usart_send_raw_str(b"\x02");
}

// TODO: Write unit tests for this function cuz it's almost guaranteed to be wrong lol.
fn usart_send_escaped_str(mut data: &[u8]) {
    while let Some(pos) = data.iter().position(|&v| v == 0x02 || v == 0x03 || v == 0x1f) {
        if pos != 0 {
            usart_send_raw_str(&data[..pos]);
        }
        usart_send_raw_str(&[0x1fu8, data[pos]]);
        data = &data[pos + 1..];
    }

    if !data.is_empty() {
        usart_send_raw_str(data);
    }
}

fn usart_send_03() {
    usart_send_raw_str(b"\x03");
}

fn usart_send_text_transmission(data: &[u8]) {
    // Replace disable_irq/enable_irq pairs with cortex_m::interrupt::free
    cortex_m::interrupt::free(|_| {
        usart_send_02();
        usart_send_escaped_str(data);
        usart_send_03();
    })
}

fn usart_send_R() {
    usart_send_text_transmission(b"R");
}

// I assume it's a signature, but I'm not sure. And I've never seen steam use it
// soo...
fn usart_send_sig_packet(data: &[u8]) {
    cortex_m::interrupt::free(|_| {
        usart_send_02();
        usart_send_escaped_str(b"[");
        usart_send_escaped_str(&data[..0x10]);
        usart_send_03();
    })
}

fn usart_send_z_packet(data: &[u8]) {
    cortex_m::interrupt::free(|_| {
        usart_send_02();
        usart_send_escaped_str(b"Z");
        usart_send_escaped_str(data);
        usart_send_03();
    })
}

fn usart_send_reset() {
    usart_send_text_transmission(b"\\RESET");
}

fn send_usart_R_if_usb_disconnected() {
    let usb_disconnected = super::is_usb_disconnected();
    if !usb_disconnected {
        for _i in 0..50_000 {
            // Do nothing.
            // TODO: Compiler barrier to avoid the loop disappearing
        }
        usart_send_R();
    }
}

fn send_usart_V_packet(data: &[u8]) {
    cortex_m::interrupt::free(|_v| {
        usart_send_02();
        usart_send_escaped_str(b"V");
        let len = usize::from(data[3]);
        usart_send_escaped_str(&data[2..2 + len]);
        usart_send_03();
    });
}

// TODO: Make this generic by clock
fn ct16b1_timer_reset() {
    let peripherals = unsafe { Peripherals::steal() };

    let backup_tcr = peripherals.CT16B1.tcr.read().bits();

    peripherals.CT16B1.tcr.write_with_zero(|v| v);
    peripherals.CT16B1.tc.write(|v| unsafe { v.tc().bits(1) });
    peripherals.CT16B1.tcr.write(|v| v.crst().reset());

    while peripherals.CT16B1.tc.read().bits() != 0 {}

    peripherals
        .CT16B1
        .tcr
        .write(|writer| unsafe { writer.bits(backup_tcr) });
}

fn init_led_ctrl() {
    let peripherals = unsafe { Peripherals::steal() };

    // Setup pinmux for ct16b1 timer.
    peripherals.IOCON.pio0_21.write(|v| v
        .func().ct16b1_mat0()
        .mode().floating());

    // Initialize ct16b1 timer.
    peripherals.SYSCON.sysahbclkctrl.modify(|_, v| v.ct16b1().enabled());

    // Set CT16B1 prescale to 0
    peripherals.CT16B1.pr.write(|v| unsafe { v.pcval().bits(0) });

    // Enable PWM mode on CT16Bn_MAT0 ()
    peripherals.CT16B1.pwmc.modify(|_, v| v.pwmen0().enabled());

    peripherals.CT16B1.mr[3].write(|v| unsafe { v.match_().bits(0xfff) });
    peripherals.CT16B1.mr[0].write(|v| unsafe { v.match_().bits(0x1000) });

    peripherals.CT16B1.mcr.modify(|_, v| v.mr3r().enabled());

    ct16b1_timer_reset();

    peripherals
        .CT16B1
        .tcr
        .modify(|_, writer| writer.cen().the_timer_counter_an());
}

fn led_advance_blink() {
    let led_blink_tick = unsafe { CUR_LED_BLINK_TICK };
    tick_led_blink((led_blink_tick + 1) % LED_BLINK_INTENSITY_LOOP.len() as u8);
}

fn tick_led_blink(tick: u8) {
    // This function has an out of bound in the original version (they send it
    // 255 in enter_programming mode, but LED_BLINK_INTENSITY_LOOP only has 128
    // values). To fix this, we default to 0. Also avoids a panic call.
    let peripherals = unsafe { Peripherals::steal() };

    peripherals.CT16B1.mr[0].write(|v| unsafe {
        v.bits(u32::from(*LED_BLINK_INTENSITY_LOOP.get(usize::from(tick)).unwrap_or(&0)))
    });
    unsafe {
        CUR_LED_BLINK_TICK = tick
    };
}

fn init_timer_32_1() {
    // TODO: Understand exactly how the timer is configured
    let peripherals = unsafe { Peripherals::steal() };

    // Enable the ct32b1 clock
    peripherals.SYSCON.sysahbclkctrl.modify(|_, v| v.ct32b1().enabled());

    // Setup CT32B1 clock rate
    let clk_rate = get_system_clock_rate();
    let clk_rate2 = clk_rate / 1000;
    peripherals.CT32B1.pr.write(|v| unsafe { v.pcval().bits(clk_rate2 - 1) });

    // Reset the TC and trigger an interrupt when MR0 matches.
    peripherals.CT32B1.mcr.modify(|_, v| v.mr0r().enabled());
    peripherals.CT32B1.mcr.modify(|_, v| v.mr0i().enabled());

    // Make MR0 match when TC reaches the value "11"
    peripherals.CT32B1.mr[0].write(|v| unsafe { v.match_().bits(11) });

    NVIC::unpend(Interrupt::CT32B1);
    unsafe { NVIC::unmask(Interrupt::CT32B1) };

    // Setup USART packet to be sent periodically.
    unsafe {
        USART_PACKET[0] = 1;
        USART_PACKET[1] = 0;
        USART_PACKET[2] = 4;
        USART_PACKET[3] = 0xc;
        USART_PACKET[4..8].copy_from_slice(&0u32.to_le_bytes());
        USART_PACKET[8] = 0;
        USART_PACKET[9] = 0;
        USART_PACKET[10] = 0;
        USART_PACKET[11] = 0;
        USART_PACKET[12..16].copy_from_slice(&0u32.to_le_bytes());
    }

    peripherals.CT32B1.tcr.modify(|_, v| v.cen().the_timer_counter_an());

    unsafe { TIMER_ENABLED = true; }
}

fn reinvoke_isp() {
    let mut core_peripherals = unsafe { CorePeripherals::steal() };
    let peripherals = unsafe { Peripherals::steal() };

    // Enable all necessary clocks for ISP operation
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.gpio().enabled());
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.ct32b1().enabled());
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.usb().enabled());
    peripherals.SYSCON.sysahbclkctrl.modify(|_, writer| writer.iocon().enabled());

    // Reset clk divisor to its default value (1)
    peripherals.SYSCON.sysahbclkdiv.reset();

    // Disable SysTick
    // (Done in a single pass by the real firmware)
    core_peripherals.SYST.disable_counter();
    core_peripherals.SYST.disable_interrupt();

    unsafe {
        asm!("
        ldr r0, =0x10001fe0
        msr msp, r0
        blx $0
        dont_return:
        wfi
        b dont_return
        " :: "{r1}"(reinvoke_isp_inner as extern fn()));
    }
}

extern fn reinvoke_isp_inner() {
    let core_peripherals = unsafe { CorePeripherals::steal() };

    for i in 0..0x1b {
        unsafe { core_peripherals.NVIC.icer[0].write(1 << i); }
    }

    iap::reinvoke_isp();
}


fn send_usart_packet_if_timer_elapsed() {
    unsafe {
        if TIMER_ENABLED && TIMER_ELAPSED {
            TIMER_ELAPSED = false;
            let mut counter = [0; 4];
            counter.copy_from_slice(&USART_PACKET[4..8]);
            let counter = u32::from_le_bytes(counter) + 1;
            USART_PACKET[4..8].copy_from_slice(&counter.to_le_bytes());

            if SHOULD_SEND_USART_PACKET {
                send_usart_V_packet(&USART_PACKET);
            }
        }
    }
}

pub fn enter_programming_mode() -> ! {
    init_usb();
    init_usart();
    send_usart_R_if_usb_disconnected();
    init_led_ctrl();
    tick_led_blink(0xff);
    init_timer_32_1();
    loop {
        if unsafe { SHOULD_REINVOKE_ISP } {
            reinvoke_isp();
        }

        // Extension: USB CDC
        crate::usb_debug_uart::usb_flush();
        send_usart_packet_if_timer_elapsed();
        cortex_m::asm::wfi();
    }
}


pub fn CT32B1() {
    let peripherals = unsafe { Peripherals::steal() };
    peripherals.CT32B1.ir.write(|v| v.mr0int().set_bit());

    unsafe {
        TIMER_ELAPSED = true;
    }
}

pub fn USB_IRQ() {
    let peripherals = unsafe { Peripherals::steal() };

    let ep_list = unsafe { slice::from_raw_parts_mut(
        peripherals.USB.epliststart.read().bits() as *mut u32, 5) };

    /*	WORKAROUND for artf32289 ROM driver BUG:
        As part of USB specification the device should respond
        with STALL condition for any unsupported setup packet. The host will send
        new setup packet/request on seeing STALL condition for EP0 instead of sending
        a clear STALL request. Current driver in ROM doesn't clear the STALL
        condition on new setup packet which should be fixed.
     */
    if peripherals.USB.devcmdstat.read().setup().bit_is_set() { // If setup packet is received
        ep_list[0] &= 0xdfff_ffff; // Clear EP0_OUT stall
        ep_list[2] &= 0xdfff_ffff; // Clear EP0_IN stall
    }

    let usb_api = RomDriver::get().usb_api();
    (usb_api.hw().isr)(unsafe { USBD_HANDLE });
}

pub fn PendSV() {
    loop {}
}

fn handle_usart_data() {
    let peripherals = unsafe { Peripherals::steal() };
    let data = peripherals.USART.rbr().read().rbr().bits();
    // TODO: Do stuff
}

pub fn USART() {
    let peripherals = unsafe { Peripherals::steal() };

    if peripherals.USART.iir().read().intid().is_receive_line_status() {
        let lsr = peripherals.USART.lsr.read();
        let oe = lsr.oe().is_active();
        let pe = lsr.pe().is_active();
        let fe = lsr.fe().is_active();
        let bi = lsr.bi().is_active();
        let rxfe = lsr.rxfe().is_erro();

        if oe || pe || fe || bi || rxfe {
            peripherals.USART.rbr().read();
        }

        if lsr.rdr().is_valid() {
            peripherals.USART.rbr().read();
        }
    }

    while peripherals.USART.lsr.read().rdr().is_valid() {
        handle_usart_data();
    }

    if peripherals.USART.ier().read().threinten().bit_is_set() {
        while peripherals.USART.lsr.read().thre().bit_is_set() {
            if let Some(val) = unsafe { USART_RING_BUFFER.dequeue() } {
                peripherals.USART.thr_mut().write(|v| unsafe { v.thr().bits(val) });
            } else {
                break;
            }
        }
        if unsafe { USART_RING_BUFFER.is_empty() } {
            peripherals.USART.ier_mut().modify(|_, v| v.threinten().clear_bit());
        }
    }
}