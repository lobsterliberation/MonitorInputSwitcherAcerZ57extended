//! DDC/CI on Apple Silicon through IOKit's IOAVService, the private API that m1ddc, MonitorControl
//! and BetterDisplay use (macOS has no public one). Intel Macs are not supported: their DDC goes
//! through IOFramebuffer instead, and recent macOS versions no longer run on them.
//!
//! Every external display has a "DCPAVServiceProxy" entry in the IORegistry (Location = External),
//! listed after the framebuffer entry ("AppleCLCD2" or "IOMobileFramebufferShim") whose
//! DisplayAttributes give its model name. Displays are identified as "av:0", "av:1"... in registry
//! order.
//!
//! IOAVServiceWriteI2C(service, 0x37, 0x51, data) sends a DDC/CI packet whose first byte (0x51, the
//! host address) goes in the "data address" argument; replies are read whole, starting with 0x6E,
//! so the packet code in `platform::ddcci` is shared with Linux. Apple Silicon drops a write now
//! and then even when it reports success, so each request is sent twice, as MonitorControl does.
//!
//! Known limit: the built-in HDMI port of some Macs (M1 Mac mini, some MacBook Pro models) does not
//! pass DDC/CI. USB-C / Thunderbolt (including DisplayPort over USB-C) does.

use crate::ddc::{DisplayInfo, INPUT_VCP};
use crate::platform::ddcci::*;
use std::ffi::{CString, c_char, c_void};
use std::thread::sleep;
use std::time::{Duration, Instant};

type CFTypeRef = *const c_void;
type IoObject = u32;
type KernReturn = i32;

#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
const DDC_ADDR: u32 = 0x37;
/// Apple Silicon needs a little longer than the standard's 40 ms before a reply.
const READ_DELAY: Duration = REPLY_DELAY.saturating_add(Duration::from_millis(10));
const WRITE_CYCLES: usize = 2;
const WRITE_GAP: Duration = Duration::from_millis(10);
const DEVICE_PREFIX: &str = "av:";

// --- CoreFoundation / IOKit ------------------------------------------------------------------------

const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const CF_NUMBER_SINT64: isize = 4; // kCFNumberSInt64Type
const ITERATE_RECURSIVELY: u32 = 1; // kIORegistryIterateRecursively

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, out: *mut c_void) -> u8;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(main_port: u32) -> IoObject;
    fn IORegistryEntryCreateIterator(
        entry: IoObject,
        plane: *const c_char,
        options: u32,
        iterator: *mut IoObject,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IORegistryEntryGetName(entry: IoObject, name: *mut c_char) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        alloc: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
}

#[cfg(target_arch = "aarch64")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOAVServiceCreateWithService(alloc: CFTypeRef, service: IoObject) -> CFTypeRef;
    fn IOAVServiceReadI2C(
        service: CFTypeRef,
        chip_address: u32,
        offset: u32,
        buf: *mut c_void,
        len: u32,
    ) -> KernReturn;
    fn IOAVServiceWriteI2C(
        service: CFTypeRef,
        chip_address: u32,
        data_address: u32,
        buf: *const c_void,
        len: u32,
    ) -> KernReturn;
}

fn av_create(entry: IoObject) -> CFTypeRef {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        IOAVServiceCreateWithService(kCFAllocatorDefault, entry)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = entry;
        std::ptr::null()
    }
}

fn av_write(service: CFTypeRef, data_address: u8, data: &[u8]) -> KernReturn {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        IOAVServiceWriteI2C(
            service,
            DDC_ADDR,
            u32::from(data_address),
            data.as_ptr().cast(),
            data.len() as u32,
        )
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (service, data_address, data);
        -1
    }
}

fn av_read(service: CFTypeRef, buf: &mut [u8]) -> KernReturn {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        IOAVServiceReadI2C(
            service,
            DDC_ADDR,
            0,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
        )
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (service, buf);
        -1
    }
}

/// An owned CoreFoundation object, released on drop.
struct Cf(CFTypeRef);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn cf_string(s: &str) -> Cf {
    let c = CString::new(s).unwrap_or_default();
    Cf(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), UTF8) })
}

fn is_type(v: CFTypeRef, type_id: usize) -> bool {
    !v.is_null() && unsafe { CFGetTypeID(v) } == type_id
}

fn string_value(v: CFTypeRef) -> Option<String> {
    if !is_type(v, unsafe { CFStringGetTypeID() }) {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) } == 0 {
        return None;
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some(String::from_utf8_lossy(&bytes).trim().to_string()).filter(|s| !s.is_empty())
}

fn number_value(v: CFTypeRef) -> Option<i64> {
    if !is_type(v, unsafe { CFNumberGetTypeID() }) {
        return None;
    }
    let mut n: i64 = 0;
    (unsafe { CFNumberGetValue(v, CF_NUMBER_SINT64, (&mut n as *mut i64).cast()) } != 0)
        .then_some(n)
}

/// A value in a dictionary (borrowed: valid while the dictionary is), or null.
fn dict_get(dict: CFTypeRef, key: &str) -> CFTypeRef {
    if !is_type(dict, unsafe { CFDictionaryGetTypeID() }) {
        return std::ptr::null();
    }
    let key = cf_string(key);
    unsafe { CFDictionaryGetValue(dict, key.0) }
}

fn property(entry: IoObject, key: &str) -> Cf {
    let key = cf_string(key);
    Cf(unsafe { IORegistryEntryCreateCFProperty(entry, key.0, kCFAllocatorDefault, 0) })
}

fn entry_name(entry: IoObject) -> String {
    let mut buf = [0 as c_char; 128]; // io_name_t
    if unsafe { IORegistryEntryGetName(entry, buf.as_mut_ptr()) } != 0 {
        return String::new();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

// --- detection -------------------------------------------------------------------------------------

#[derive(Default)]
struct Attributes {
    model: String,
    mfg: String,
    serial: String,
}

/// The monitor described by a framebuffer entry's DisplayAttributes > ProductAttributes.
fn attributes(framebuffer: IoObject) -> Attributes {
    let display = property(framebuffer, "DisplayAttributes");
    let product = dict_get(display.0, "ProductAttributes");
    let text = |key| string_value(dict_get(product, key)).unwrap_or_default();
    let serial = string_value(dict_get(product, "AlphanumericSerialNumber"))
        .or_else(|| {
            number_value(dict_get(product, "SerialNumber"))
                .filter(|n| *n != 0)
                .map(|n| n.to_string())
        })
        .unwrap_or_default();
    Attributes {
        model: text("ProductName"),
        mfg: text("ManufacturerID"),
        serial,
    }
}

struct AvDisplay {
    service: Cf,
    attrs: Attributes,
}

/// The external displays' AV services, in IORegistry order.
fn enumerate() -> Vec<AvDisplay> {
    let mut out = Vec::new();
    unsafe {
        let root = IORegistryGetRootEntry(0);
        let mut iter: IoObject = 0;
        if IORegistryEntryCreateIterator(
            root,
            c"IOService".as_ptr(),
            ITERATE_RECURSIVELY,
            &mut iter,
        ) != 0
        {
            IOObjectRelease(root);
            return out;
        }
        let mut framebuffer: Option<Attributes> = None;
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            match entry_name(entry).as_str() {
                "AppleCLCD2" | "IOMobileFramebufferShim" => framebuffer = Some(attributes(entry)),
                "DCPAVServiceProxy" => {
                    let location = property(entry, "Location");
                    if string_value(location.0).as_deref() == Some("External") {
                        let service = av_create(entry);
                        if !service.is_null() {
                            out.push(AvDisplay {
                                service: Cf(service),
                                attrs: framebuffer.take().unwrap_or_default(),
                            });
                        }
                    }
                }
                _ => {}
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
        IOObjectRelease(root);
    }
    out
}

/// All external displays, including ones whose DDC/CI channel does not respond (valid = false).
/// The built-in screen is never listed: it has no inputs to switch.
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    if cfg!(not(target_arch = "aarch64")) {
        return Err("DDC/CI on Intel Macs is not supported (Apple Silicon only)".into());
    }
    let displays: Vec<DisplayInfo> = enumerate()
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let mut bus = Bus {
                service: d.service,
                last: None,
            };
            let valid = bus.get_vcp(INPUT_VCP).is_ok();
            DisplayInfo {
                bus: format!("{DEVICE_PREFIX}{i}"),
                model: d.attrs.model,
                mfg: d.attrs.mfg,
                serial: d.attrs.serial,
                connector: String::new(),
                valid,
            }
        })
        .collect();
    if displays.is_empty() {
        return Err("No external monitor found (the built-in screen is ignored)".into());
    }
    Ok(displays)
}

/// Whether a configured `MonitorDevice` is a macOS one ("av:0").
pub fn is_device(device: &str) -> bool {
    device
        .strip_prefix(DEVICE_PREFIX)
        .is_some_and(|n| n.parse::<usize>().is_ok())
}

pub fn open(device: &str) -> Result<Bus, String> {
    let index: usize = device
        .strip_prefix(DEVICE_PREFIX)
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("{device} is not a macOS display id"))?;
    enumerate()
        .into_iter()
        .nth(index)
        .map(|d| Bus {
            service: d.service,
            last: None,
        })
        .ok_or_else(|| format!("Monitor {device} is not connected"))
}

// --- I/O -------------------------------------------------------------------------------------------

fn io_error(rc: KernReturn) -> String {
    format!(
        "The monitor does not answer DDC/CI (error 0x{:08X}). Is DDC/CI enabled in its on-screen \
         menu? On some Macs the built-in HDMI port does not pass DDC/CI: use USB-C/Thunderbolt.",
        rc as u32
    )
}

/// One external display's DDC/CI channel, keeping the inter-command gap the monitor needs.
pub struct Bus {
    service: Cf,
    last: Option<Instant>,
}

// The IOAVService object is only ever used by one thread at a time (the DDC worker owns the Bus).
unsafe impl Send for Bus {}

impl Bus {
    fn wait_gap(&self) {
        if let Some(elapsed) = self.last.map(|t| t.elapsed())
            && elapsed < COMMAND_GAP
        {
            sleep(COMMAND_GAP - elapsed);
        }
    }

    /// Sends a DDC/CI packet (starting with the 0x51 host address).
    fn write(&self, packet: &[u8]) -> Result<(), String> {
        let (address, data) = packet.split_first().ok_or("Empty DDC packet")?;
        let mut result = Err(String::new());
        for cycle in 0..WRITE_CYCLES {
            if cycle > 0 {
                sleep(WRITE_GAP);
            }
            match av_write(self.service.0, *address, data) {
                0 => result = Ok(()),
                rc if result.is_err() => result = Err(io_error(rc)),
                _ => {}
            }
        }
        result
    }

    fn read(&self, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        match av_read(self.service.0, &mut buf) {
            0 => Ok(buf),
            rc => Err(io_error(rc)),
        }
    }

    /// Runs one request/reply exchange with retries, keeping the gap between commands.
    fn exchange<T>(&mut self, op: impl Fn(&Bus) -> Result<T, String>) -> Result<T, String> {
        let mut last_err = String::new();
        for _ in 0..TRIES {
            self.wait_gap();
            let result = op(self);
            self.last = Some(Instant::now());
            match result {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    pub fn get_vcp(&mut self, vcp: u8) -> Result<u32, String> {
        self.exchange(|bus| {
            bus.write(&get_vcp_packet(vcp))?;
            sleep(READ_DELAY);
            parse_vcp_reply(vcp, &bus.read(11)?)
        })
    }

    /// The monitor's capabilities string (a few seconds: it arrives in 32-byte fragments).
    pub fn capabilities(&mut self) -> Result<String, String> {
        let mut text: Vec<u8> = Vec::new();
        loop {
            let offset = u16::try_from(text.len()).map_err(|_| "Capabilities string too long")?;
            let data = self.exchange(|bus| {
                bus.write(&caps_request_packet(offset))?;
                sleep(CAPS_DELAY.max(READ_DELAY));
                let (got, data) = parse_caps_fragment(&bus.read(39)?)?;
                if got != offset {
                    return Err(format!("Capabilities fragment at {got}, expected {offset}"));
                }
                Ok(data)
            })?;
            if data.is_empty() || text.len() + data.len() > CAPS_MAX_LEN {
                break;
            }
            text.extend_from_slice(&data);
        }
        while text.last() == Some(&0) {
            text.pop();
        }
        if text.is_empty() {
            return Err("The monitor returned no capabilities".into());
        }
        Ok(String::from_utf8_lossy(&text).trim().to_string())
    }

    pub fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String> {
        self.exchange(|bus| bus.write(&set_vcp_packet(vcp, value)))
    }
}
