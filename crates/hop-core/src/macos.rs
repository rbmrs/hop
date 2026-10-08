//! macOS backend: DDC/CI over IOAVService on Apple Silicon. These are
//! private IOKit symbols, the same ones m1ddc uses.

use std::ffi::{CStr, c_char, c_void};
use std::thread::sleep;
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

use crate::caps::INPUT_SOURCE;
use crate::ddc::{self, ReplyError};
use crate::monitor::{DdcBackend, Display, Error};

type IoObject = u32;

const MAIN_PORT_DEFAULT: u32 = 0;
const ITERATE_RECURSIVELY: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(main_port: u32) -> IoObject;
    fn IORegistryEntryCreateIterator(
        entry: IoObject,
        plane: *const c_char,
        options: u32,
        iter: *mut IoObject,
    ) -> i32;
    fn IOIteratorNext(iter: IoObject) -> IoObject;
    fn IORegistryEntryGetName(entry: IoObject, name: *mut c_char) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        alloc: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObject) -> i32;
    fn IOAVServiceCreateWithService(alloc: CFTypeRef, service: IoObject) -> CFTypeRef;
    fn IOAVServiceReadI2C(
        service: CFTypeRef,
        chip: u32,
        offset: u32,
        buf: *mut c_void,
        len: u32,
    ) -> i32;
    fn IOAVServiceWriteI2C(
        service: CFTypeRef,
        chip: u32,
        offset: u32,
        buf: *const c_void,
        len: u32,
    ) -> i32;
}

/// DDC/CI needs at least 40 ms between a request and reading its reply.
const REPLY_DELAY: Duration = Duration::from_millis(50);
/// Extra wait when the display answers with a null message (busy).
const BUSY_DELAY: Duration = Duration::from_millis(200);
const RETRIES: usize = 5;
/// The monitor sends null replies for about 3 s after a switch; keep retrying
/// those for this long before giving up.
const BUSY_WINDOW: Duration = Duration::from_secs(4);
/// MCCS strings are well under 1 KiB; stop a monitor that never ends one.
const MAX_CAPS_LEN: usize = 8 * 1024;

pub struct MacBackend {
    services: Vec<(Display, CFType)>,
}

impl MacBackend {
    /// Finds every external display with an IOAVService.
    pub fn new() -> Self {
        Self {
            services: external_services(),
        }
    }

    fn service(&self, display: &Display) -> Result<&CFType, Error> {
        self.services
            .iter()
            .find(|(d, _)| d == display)
            .map(|(_, s)| s)
            .ok_or_else(|| Error::Transport(format!("{} is no longer connected", display.name)))
    }
}

impl Default for MacBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DdcBackend for MacBackend {
    fn list_displays(&self) -> Result<Vec<Display>, Error> {
        Ok(self.services.iter().map(|(d, _)| d.clone()).collect())
    }

    fn capabilities(&self, display: &Display) -> Result<String, Error> {
        let av = self.service(display)?;
        let mut caps = Vec::new();
        loop {
            if caps.len() > MAX_CAPS_LEN {
                return Err(Error::Transport("capabilities string never ended".into()));
            }
            let offset = caps.len() as u16;
            let chunk = transact(
                av,
                &ddc::capabilities_request(offset),
                ddc::CAPS_REPLY_LEN,
                |r| ddc::decode_capabilities_reply(r, offset).map(<[u8]>::to_vec),
            )?;
            if chunk.is_empty() {
                break;
            }
            caps.extend_from_slice(&chunk);
        }
        // Some monitors end the string with a NUL.
        Ok(String::from_utf8_lossy(&caps)
            .trim_end_matches('\0')
            .to_string())
    }

    fn get_input(&self, display: &Display) -> Result<u8, Error> {
        let av = self.service(display)?;
        let value = transact(
            av,
            &ddc::get_vcp_request(INPUT_SOURCE),
            ddc::VCP_REPLY_LEN,
            |r| ddc::decode_vcp_reply(r, INPUT_SOURCE),
        )?;
        Ok(value.low_byte())
    }

    fn set_input(&self, display: &Display, code: u8) -> Result<(), Error> {
        let av = self.service(display)?;
        write(av, &ddc::set_vcp_request(INPUT_SOURCE, code as u16))
    }
}

/// Sends a request and decodes the reply. Invalid replies are retried
/// `RETRIES` times; null replies (busy) are retried for up to `BUSY_WINDOW`.
fn transact<T>(
    av: &CFType,
    request: &[u8],
    reply_len: usize,
    decode: impl Fn(&[u8]) -> Result<T, ReplyError>,
) -> Result<T, Error> {
    let busy_until = Instant::now() + BUSY_WINDOW;
    let mut failures = 0;
    loop {
        let result = write(av, request).and_then(|()| {
            sleep(REPLY_DELAY);
            let reply = read(av, reply_len)?;
            decode(&reply).map_err(Error::Reply)
        });
        let e = match result {
            Ok(value) => return Ok(value),
            Err(e) => e,
        };
        let busy = matches!(e, Error::Reply(ReplyError::Null));
        if !busy {
            failures += 1;
        }
        if failures >= RETRIES || (busy && Instant::now() >= busy_until) {
            return Err(e);
        }
        sleep(if busy { BUSY_DELAY } else { REPLY_DELAY });
    }
}

fn write(av: &CFType, data: &[u8]) -> Result<(), Error> {
    let rc = unsafe {
        IOAVServiceWriteI2C(
            av.as_CFTypeRef(),
            ddc::DDC_CHIP as u32,
            ddc::HOST as u32,
            data.as_ptr().cast(),
            data.len() as u32,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Transport(format!(
            "I2C write failed (IOReturn 0x{rc:08X})"
        )))
    }
}

fn read(av: &CFType, len: usize) -> Result<Vec<u8>, Error> {
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        IOAVServiceReadI2C(
            av.as_CFTypeRef(),
            ddc::DDC_CHIP as u32,
            ddc::HOST as u32,
            buf.as_mut_ptr().cast(),
            len as u32,
        )
    };
    if rc == 0 {
        Ok(buf)
    } else {
        Err(Error::Transport(format!(
            "I2C read failed (IOReturn 0x{rc:08X})"
        )))
    }
}

/// Walks the IOService plane. Each external DCPAVServiceProxy follows the
/// framebuffer entry that holds its display's EDID-derived attributes.
fn external_services() -> Vec<(Display, CFType)> {
    let mut found = Vec::new();
    let mut last_display: Option<Display> = None;
    unsafe {
        let root = IORegistryGetRootEntry(MAIN_PORT_DEFAULT);
        let mut iter: IoObject = 0;
        if IORegistryEntryCreateIterator(
            root,
            c"IOService".as_ptr(),
            ITERATE_RECURSIVELY,
            &mut iter,
        ) != 0
        {
            IOObjectRelease(root);
            return found;
        }
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            if let Some(display) = display_attributes(entry) {
                last_display = Some(display);
            } else if entry_name(entry) == "DCPAVServiceProxy" {
                // Consume the attributes on every proxy, so a built-in panel's
                // attributes never label the next external monitor.
                let display = last_display.take();
                let external = string_property(entry, "Location").as_deref() == Some("External");
                let av = if external {
                    IOAVServiceCreateWithService(kCFAllocatorDefault as CFTypeRef, entry)
                } else {
                    std::ptr::null()
                };
                if !av.is_null() {
                    let display = display.unwrap_or_else(|| Display {
                        name: "External display".into(),
                        serial: None,
                    });
                    found.push((display, CFType::wrap_under_create_rule(av)));
                }
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
        IOObjectRelease(root);
    }
    found
}

unsafe fn entry_name(entry: IoObject) -> String {
    let mut name = [0 as c_char; 128];
    if unsafe { IORegistryEntryGetName(entry, name.as_mut_ptr()) } != 0 {
        return String::new();
    }
    unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn property(entry: IoObject, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let value = unsafe {
        IORegistryEntryCreateCFProperty(
            entry,
            key.as_CFTypeRef(),
            kCFAllocatorDefault as CFTypeRef,
            0,
        )
    };
    (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

unsafe fn string_property(entry: IoObject, key: &str) -> Option<String> {
    unsafe { property(entry, key) }?
        .downcast::<CFString>()
        .map(|s| s.to_string())
}

/// Reads DisplayAttributes → ProductAttributes → ProductName / serial.
unsafe fn display_attributes(entry: IoObject) -> Option<Display> {
    let attrs = as_dict(&unsafe { property(entry, "DisplayAttributes") }?)?;
    let product = as_dict(&dict_get(&attrs, "ProductAttributes")?)?;
    let name = dict_get(&product, "ProductName")?
        .downcast::<CFString>()?
        .to_string();
    let serial = dict_get(&product, "AlphanumericSerialNumber")
        .and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string());
    Some(Display { name, serial })
}

fn as_dict(value: &CFType) -> Option<CFDictionary<CFString, CFType>> {
    let dict = value.downcast::<CFDictionary>()?;
    Some(unsafe { CFDictionary::wrap_under_get_rule(dict.as_concrete_TypeRef()) })
}

fn dict_get(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<CFType> {
    dict.find(CFString::new(key)).map(|v| v.clone())
}
