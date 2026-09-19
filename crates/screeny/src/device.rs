//! An owned view of a device and its advertised metadata.
//!
//! `screeny_proto::txt::DeviceInfo` borrows from the bytes it parsed, which is
//! right for the firmware and wrong for a host tool that wants to keep a
//! device around. [`DeviceInfo`] here is the owned mirror: it is still
//! **parsed by proto**, from the DNS-SD TXT wire format, whether those bytes
//! came from an mDNS TXT record or from a `GET_INFO` reply body - which is
//! the point of spec 6.6 making them the same bytes.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use screeny_proto::txt;
use screeny_proto::{DEFAULT_CONTROL_PORT, DEFAULT_FRAME_PORT, MAX_PIXEL_PAYLOAD};

use crate::error::{Error, Result};

/// What a device says about itself (spec 5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// TXT schema version.
    pub txtvers: u16,
    /// Wire protocol versions supported, comma separated.
    pub proto: String,
    /// Panel width in pixels.
    pub w: u16,
    /// Panel height in pixels.
    pub h: u16,
    /// Codec ids the device can decode, in the device's preference order.
    pub codecs: Vec<u8>,
    /// Largest pixel payload the device accepts.
    pub mtu: u16,
    /// Control UDP port.
    pub ctrl: u16,
    /// Firmware version.
    pub fw: String,
    /// Stable short device id (MAC suffix).
    pub id: String,
    /// Friendly name.
    pub name: String,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        let d = txt::DeviceInfo::DEFAULT;
        DeviceInfo {
            txtvers: d.txtvers,
            proto: d.proto.to_string(),
            w: d.w,
            h: d.h,
            codecs: d.codec_ids().collect(),
            mtu: d.mtu,
            ctrl: d.ctrl,
            fw: d.fw.to_string(),
            id: d.id.to_string(),
            name: d.name.to_string(),
        }
    }
}

impl DeviceInfo {
    /// Parse the DNS-SD TXT wire format (spec 5.2 / 6.6).
    ///
    /// # Errors
    ///
    /// [`Error::Metadata`] if a required key is missing or malformed. Spec 5.2
    /// requires ignoring such a service rather than guessing.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let d = txt::DeviceInfo::parse(bytes)
            .map_err(|e| Error::Metadata(format!("{e:?} while parsing TXT record")))?;
        Ok(DeviceInfo {
            txtvers: d.txtvers,
            proto: d.proto.to_string(),
            w: d.w,
            h: d.h,
            codecs: d.codec_ids().collect(),
            mtu: d.mtu,
            ctrl: d.ctrl,
            fw: d.fw.to_string(),
            id: d.id.to_string(),
            name: d.name.to_string(),
        })
    }

    /// Assemble key/value pairs into TXT wire bytes and parse them.
    ///
    /// This is how an mDNS browse result becomes a `DeviceInfo`: the daemon
    /// hands back decoded properties, and rather than re-implement the key
    /// semantics here they are re-encoded and handed to proto's parser, which
    /// is the same code path a `GET_INFO` reply takes.
    ///
    /// # Errors
    ///
    /// As [`DeviceInfo::parse`].
    pub fn from_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a [u8])>) -> Result<Self> {
        let mut bytes = Vec::with_capacity(256);
        // `txtvers` must come first (RFC 6763 6.5), so collect and sort it up.
        let mut items: Vec<(&str, &[u8])> = pairs.collect();
        items.sort_by_key(|(k, _)| *k != "txtvers");
        for (k, v) in items {
            let n = k.len() + 1 + v.len();
            if n > 255 {
                return Err(Error::Metadata(format!("TXT key {k:?} is too long")));
            }
            bytes.push(n as u8);
            bytes.extend_from_slice(k.as_bytes());
            bytes.push(b'=');
            bytes.extend_from_slice(v);
        }
        Self::parse(&bytes)
    }

    /// Re-encode to the TXT wire format.
    ///
    /// # Errors
    ///
    /// [`Error::Metadata`] if the values do not fit proto's writer.
    pub fn to_txt(&self) -> Result<Vec<u8>> {
        let codecs = self
            .codecs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let d = txt::DeviceInfo {
            txtvers: self.txtvers,
            proto: &self.proto,
            w: self.w,
            h: self.h,
            codecs: &codecs,
            mtu: self.mtu,
            ctrl: self.ctrl,
            fw: &self.fw,
            id: &self.id,
            name: &self.name,
        };
        let mut out = vec![0u8; 512];
        let n = d
            .write(&mut out)
            .map_err(|e| Error::Metadata(format!("{e:?} while writing TXT record")))?;
        out.truncate(n);
        Ok(out)
    }

    /// True if the device advertises this codec.
    #[must_use]
    pub fn supports(&self, id: u8) -> bool {
        self.codecs.contains(&id)
    }

    /// The device's most preferred codec that this sender can also produce
    /// (spec 9.4 step 3).
    #[must_use]
    pub fn best_codec(&self, mine: &[u8]) -> Option<u8> {
        self.codecs.iter().copied().find(|c| mine.contains(c))
    }

    /// Codecs both sides can do, in the device's preference order.
    #[must_use]
    pub fn common_codecs(&self, mine: &[u8]) -> Vec<u8> {
        self.codecs
            .iter()
            .copied()
            .filter(|c| mine.contains(c))
            .collect()
    }

    /// The pixel-payload budget to use: the device's `mtu`, never above the
    /// protocol's own ceiling.
    #[must_use]
    pub fn budget(&self) -> usize {
        (self.mtu as usize).min(MAX_PIXEL_PAYLOAD)
    }

    /// True if the device speaks protocol version 1.
    #[must_use]
    pub fn speaks_v1(&self) -> bool {
        self.proto.split(',').any(|v| v.trim() == "1")
    }
}

impl fmt::Display for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}x{}, fw {}, mtu {}, codecs {:?})",
            if self.name.is_empty() {
                &self.id
            } else {
                &self.name
            },
            self.w,
            self.h,
            self.fw,
            self.mtu,
            self.codecs
        )
    }
}

/// A device we can talk to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// DNS-SD instance name, or a synthesised one when `--addr` was used.
    pub instance: String,
    /// Host name from the `SRV` record, if discovery provided one.
    pub host: Option<String>,
    /// Frame port address (the `SRV` target and port).
    pub frame: SocketAddr,
    /// Control port address (from the `ctrl=` TXT key).
    pub control: SocketAddr,
    /// Every address the service resolved to.
    pub addresses: Vec<IpAddr>,
    /// Advertised metadata, if we have any yet.
    pub info: Option<DeviceInfo>,
}

impl Device {
    /// A device from a bare `IP[:port]`, skipping discovery entirely.
    ///
    /// The control port is the frame port plus one, matching the defaults
    /// (49374/49375); a `GET_INFO` handshake replaces that guess with the
    /// advertised `ctrl=` value.
    #[must_use]
    pub fn from_addr(addr: SocketAddr) -> Self {
        let frame = addr;
        let ctrl = if frame.port() == DEFAULT_FRAME_PORT {
            DEFAULT_CONTROL_PORT
        } else {
            frame.port().wrapping_add(1)
        };
        let mut control = frame;
        control.set_port(ctrl);
        Device {
            instance: frame.to_string(),
            host: None,
            frame,
            control,
            addresses: vec![frame.ip()],
            info: None,
        }
    }

    /// Adopt metadata learned from `GET_INFO`, including the authoritative
    /// control port.
    pub fn apply(&mut self, info: DeviceInfo) {
        self.control.set_port(info.ctrl);
        self.info = Some(info);
    }

    /// A short label for logs.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.info {
            Some(i) if !i.name.is_empty() => format!("{} at {}", i.name, self.frame),
            _ => format!("{} at {}", self.instance, self.frame),
        }
    }
}
