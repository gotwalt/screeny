//! DNS-SD TXT record wire format.
//!
//! Spec: `docs/design/protocol-v1.md` sections 5.2 and 6.6. The same bytes are
//! the mDNS TXT record and the `GET_INFO` reply body, which is the point: one
//! table in the firmware, one parser in the sender, and a sender handed a bare
//! IP address gets identical metadata to one that browsed mDNS.
//!
//! The format is a sequence of length-prefixed strings (RFC 6763 section 6.1):
//! a `u8` length, then that many bytes of `key` or `key=value`.
//!
//! ```
//! # use screeny_proto::txt::{self, DeviceInfo};
//! let mut buf = [0u8; 256];
//! let n = DeviceInfo {
//!     codecs: "16,17,40,2,127",
//!     id: "a4cf12",
//!     name: "Desk panel",
//!     ..DeviceInfo::DEFAULT
//! }
//! .write(&mut buf)
//! .unwrap();
//!
//! let info = DeviceInfo::parse(&buf[..n]).unwrap();
//! assert_eq!(info.w, 64);
//! assert!(info.supports(0x28));
//! assert_eq!(txt::find(&buf[..n], "name"), Some(&b"Desk panel"[..]));
//! ```

use crate::packet::BuildError;

/// TXT schema version this crate writes.
pub const TXTVERS: u16 = 1;

/// One `key` or `key=value` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Bytes before the first `=`, or the whole string if there is none.
    pub key: &'a [u8],
    /// Bytes after the first `=`. `None` when the entry had no `=` at all,
    /// which RFC 6763 section 6.4 defines as a present-but-valueless key.
    pub value: Option<&'a [u8]>,
}

/// Iterator over the entries of a TXT record.
///
/// Total by construction: a length byte that overruns the buffer ends the
/// iteration, and a zero-length string is skipped as RFC 6763 section 6.1
/// requires.
#[derive(Debug, Clone)]
pub struct Iter<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Iter<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Entry<'a>> {
        loop {
            let (&n, tail) = self.rest.split_first()?;
            let n = n as usize;
            if tail.len() < n {
                // Truncated record. Stop rather than guess.
                self.rest = &[];
                return None;
            }
            let (s, tail) = tail.split_at(n);
            self.rest = tail;
            if s.is_empty() {
                continue; // empty strings are ignored, not entries
            }
            return Some(match s.iter().position(|&b| b == b'=') {
                Some(i) => Entry {
                    key: &s[..i],
                    value: Some(&s[i + 1..]),
                },
                None => Entry {
                    key: s,
                    value: None,
                },
            });
        }
    }
}

/// Iterate a TXT record's entries.
#[must_use]
pub fn iter(bytes: &[u8]) -> Iter<'_> {
    Iter { rest: bytes }
}

/// The value of the first entry with this key, if it had one.
///
/// Keys are compared byte for byte; the spec says they are lowercase ASCII.
#[must_use]
pub fn find<'a>(bytes: &'a [u8], key: &str) -> Option<&'a [u8]> {
    iter(bytes).find(|e| e.key == key.as_bytes())?.value
}

/// Why a TXT record could not be read as a [`DeviceInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TxtError {
    /// A key section 5.2 marks required was absent or valueless.
    /// Such a service MUST be ignored.
    MissingKey(&'static str),
    /// A value was not valid UTF-8, or not the number the key calls for.
    BadValue(&'static str),
}

/// Parse a decimal `u16`. Strict: no sign, no spaces, no overflow, not empty.
fn dec_u16(b: &[u8]) -> Option<u16> {
    if b.is_empty() || b.len() > 5 {
        return None;
    }
    let mut v: u16 = 0;
    for &c in b {
        let d = c.checked_sub(b'0')?;
        if d > 9 {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(d as u16)?;
    }
    Some(v)
}

/// Write a decimal `u16` into `out`, returning how many bytes it took.
fn put_u16(v: u16, out: &mut [u8; 5]) -> usize {
    let mut tmp = [0u8; 5];
    let mut n = 0;
    let mut v = v;
    loop {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        out[i] = tmp[n - 1 - i];
    }
    n
}

/// The device metadata carried in the TXT record and in `GET_INFO`.
///
/// Borrowed throughout, so the firmware can build one from `&'static str`
/// constants and a sender can point it at the datagram it just received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo<'a> {
    /// TXT schema version. Written first, as RFC 6763 section 6.5 requires.
    pub txtvers: u16,
    /// Wire protocol versions supported, comma-separated. Required.
    pub proto: &'a str,
    /// Panel width in pixels. Required.
    pub w: u16,
    /// Panel height in pixels. Required.
    pub h: u16,
    /// Decimal codec ids, comma-separated, most preferred first. Required.
    pub codecs: &'a str,
    /// Max pixel payload the device accepts.
    pub mtu: u16,
    /// Control UDP port. Required.
    pub ctrl: u16,
    /// Firmware version.
    pub fw: &'a str,
    /// Stable short device id (MAC suffix).
    pub id: &'a str,
    /// Friendly name, UTF-8, may contain spaces.
    pub name: &'a str,
}

impl DeviceInfo<'static> {
    /// A v1 device's defaults: the geometry and limits this crate is built
    /// for, with the identity fields left empty.
    pub const DEFAULT: DeviceInfo<'static> = DeviceInfo {
        txtvers: TXTVERS,
        proto: "1",
        w: crate::W as u16,
        h: crate::H as u16,
        codecs: "",
        mtu: crate::MAX_PIXEL_PAYLOAD as u16,
        ctrl: crate::DEFAULT_CONTROL_PORT,
        fw: "",
        id: "",
        name: "",
    };
}

impl Default for DeviceInfo<'static> {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl<'a> DeviceInfo<'a> {
    /// Read a TXT record.
    ///
    /// Unknown keys are ignored and missing *optional* keys take the
    /// [`DeviceInfo::DEFAULT`] value, as section 5.2 requires of a sender.
    ///
    /// # Errors
    ///
    /// [`TxtError::MissingKey`] if `proto`, `w`, `h`, `codecs` or `ctrl` is
    /// absent - section 5.2 says to ignore such a service - and
    /// [`TxtError::BadValue`] if a value is malformed.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, TxtError> {
        let req_str = |k: &'static str| -> Result<&'a str, TxtError> {
            let v = find(bytes, k).ok_or(TxtError::MissingKey(k))?;
            core::str::from_utf8(v).map_err(|_| TxtError::BadValue(k))
        };
        let req_num = |k: &'static str| -> Result<u16, TxtError> {
            let v = find(bytes, k).ok_or(TxtError::MissingKey(k))?;
            dec_u16(v).ok_or(TxtError::BadValue(k))
        };
        let opt_str = |k: &'static str, d: &'a str| -> Result<&'a str, TxtError> {
            match find(bytes, k) {
                Some(v) => core::str::from_utf8(v).map_err(|_| TxtError::BadValue(k)),
                None => Ok(d),
            }
        };
        let opt_num = |k: &'static str, d: u16| -> Result<u16, TxtError> {
            match find(bytes, k) {
                Some(v) => dec_u16(v).ok_or(TxtError::BadValue(k)),
                None => Ok(d),
            }
        };
        Ok(DeviceInfo {
            txtvers: opt_num("txtvers", TXTVERS)?,
            proto: req_str("proto")?,
            w: req_num("w")?,
            h: req_num("h")?,
            codecs: req_str("codecs")?,
            mtu: opt_num("mtu", DeviceInfo::DEFAULT.mtu)?,
            ctrl: req_num("ctrl")?,
            fw: opt_str("fw", "")?,
            id: opt_str("id", "")?,
            name: opt_str("name", "")?,
        })
    }

    /// Write the TXT record, keys in the order section 5.2 lists them.
    ///
    /// Returns the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`BuildError::BufferTooSmall`] if `out` cannot hold the record,
    /// [`BuildError::TooLong`] if any single `key=value` would exceed the
    /// 255-byte limit of a TXT string.
    pub fn write(&self, out: &mut [u8]) -> Result<usize, BuildError> {
        let mut w = Writer { out, at: 0 };
        let mut num = [0u8; 5];
        let n = put_u16(self.txtvers, &mut num);
        w.push(b"txtvers", &num[..n])?;
        w.push(b"proto", self.proto.as_bytes())?;
        let n = put_u16(self.w, &mut num);
        w.push(b"w", &num[..n])?;
        let n = put_u16(self.h, &mut num);
        w.push(b"h", &num[..n])?;
        w.push(b"codecs", self.codecs.as_bytes())?;
        let n = put_u16(self.mtu, &mut num);
        w.push(b"mtu", &num[..n])?;
        let n = put_u16(self.ctrl, &mut num);
        w.push(b"ctrl", &num[..n])?;
        w.push(b"fw", self.fw.as_bytes())?;
        w.push(b"id", self.id.as_bytes())?;
        w.push(b"name", self.name.as_bytes())?;
        Ok(w.at)
    }

    /// The `codecs` list as ids. Entries that are not a decimal `u8` are
    /// skipped, because a sender must tolerate a device newer than itself.
    #[must_use]
    pub fn codec_ids(&self) -> CodecIds<'a> {
        CodecIds {
            rest: self.codecs.as_bytes(),
        }
    }

    /// True if the device advertised this codec id.
    #[must_use]
    pub fn supports(&self, id: u8) -> bool {
        self.codec_ids().any(|c| c == id)
    }

    /// The first id in `codecs` that `mine` also contains, honouring the
    /// device's preference order. This is step 3 of section 9.4.
    #[must_use]
    pub fn best_codec(&self, mine: &[u8]) -> Option<u8> {
        self.codec_ids().find(|c| mine.contains(c))
    }
}

/// Iterator over the decimal ids in a `codecs` value.
#[derive(Debug, Clone)]
pub struct CodecIds<'a> {
    rest: &'a [u8],
}

impl Iterator for CodecIds<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            let (field, tail) = match self.rest.iter().position(|&b| b == b',') {
                Some(i) => (&self.rest[..i], &self.rest[i + 1..]),
                None => (self.rest, &self.rest[self.rest.len()..]),
            };
            self.rest = tail;
            if let Some(v) = dec_u16(field) {
                if v <= 255 {
                    return Some(v as u8);
                }
            }
            // Not a codec id we understand; keep going.
        }
    }
}

struct Writer<'b> {
    out: &'b mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn push(&mut self, key: &[u8], value: &[u8]) -> Result<(), BuildError> {
        let n = key
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(value.len()))
            .ok_or(BuildError::TooLong)?;
        if n > 255 {
            return Err(BuildError::TooLong);
        }
        let end = self
            .at
            .checked_add(1)
            .and_then(|a| a.checked_add(n))
            .ok_or(BuildError::TooLong)?;
        if end > self.out.len() {
            return Err(BuildError::BufferTooSmall);
        }
        self.out[self.at] = n as u8;
        let k = self.at + 1;
        self.out[k..k + key.len()].copy_from_slice(key);
        self.out[k + key.len()] = b'=';
        let v = k + key.len() + 1;
        self.out[v..v + value.len()].copy_from_slice(value);
        self.at = end;
        Ok(())
    }
}
