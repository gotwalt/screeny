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
    /// The `codecs=` value exactly as advertised. Kept because a device that
    /// advertises something this sender cannot parse - the bring-up firmware
    /// says `codecs=raw` - is much easier to diagnose when the tool can show
    /// what it actually saw.
    pub codecs_raw: String,
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
            codecs_raw: d.codecs.to_string(),
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
            codecs_raw: d.codecs.to_string(),
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
        let codecs = if self.codecs.is_empty() {
            self.codecs_raw.clone()
        } else {
            self.codecs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn txt(pairs: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in pairs {
            out.push(p.len() as u8);
            out.extend_from_slice(p.as_bytes());
        }
        out
    }

    /// What the card 001 bring-up firmware actually advertises today, read
    /// off the wire with `dns-sd -L`: no `txtvers`, no `mtu`, and a `codecs`
    /// value from before the codec table existed.
    ///
    /// A sender must handle this gracefully - discover the device, show what
    /// it saw, and refuse to stream - rather than crash or, worse, guess a
    /// codec and send it.
    #[test]
    fn the_bring_up_firmwares_txt_record_parses_but_offers_no_codecs() {
        let bytes = txt(&["proto=1", "w=64", "h=32", "ctrl=49375", "codecs=raw"]);
        let info = DeviceInfo::parse(&bytes).expect("parses");
        assert!(info.speaks_v1());
        assert_eq!(info.w, 64);
        assert_eq!(info.ctrl, 49375);
        assert!(info.codecs.is_empty(), "\"raw\" is not a codec id");
        assert_eq!(info.codecs_raw, "raw");
        assert_eq!(info.best_codec(&screeny_proto::dec::SUPPORTED_CODECS), None);
        // The raw value survives a round trip, so what gets printed is what
        // was advertised.
        let again = DeviceInfo::parse(&info.to_txt().unwrap()).unwrap();
        assert_eq!(again.codecs_raw, "raw");
    }

    #[test]
    fn a_full_txt_record_round_trips() {
        let bytes = txt(&[
            "txtvers=1",
            "proto=1",
            "w=64",
            "h=32",
            "codecs=16,17,40,2,127",
            "mtu=1464",
            "ctrl=49375",
            "fw=0.1.0",
            "id=a4cf12",
            "name=Desk panel",
        ]);
        let info = DeviceInfo::parse(&bytes).expect("parses");
        assert_eq!(info.codecs, screeny_proto::dec::SUPPORTED_CODECS.to_vec());
        assert_eq!(info.name, "Desk panel");
        assert_eq!(info.budget(), 1464);
        assert_eq!(info.best_codec(&[0x28, 0x02]), Some(0x28));
        assert_eq!(info.common_codecs(&[0x02, 0x10]), vec![0x10, 0x02]);
        assert_eq!(DeviceInfo::parse(&info.to_txt().unwrap()).unwrap(), info);
    }

    /// A required key missing means the service must be ignored (spec 5.2).
    #[test]
    fn a_service_missing_a_required_key_is_rejected() {
        const KEYS: [&str; 5] = ["proto=1", "w=64", "h=32", "ctrl=49375", "codecs=16"];
        for dropped in KEYS {
            let kept: Vec<&str> = KEYS.into_iter().filter(|k| *k != dropped).collect();
            assert!(
                DeviceInfo::parse(&txt(&kept)).is_err(),
                "accepted a record with no {dropped}"
            );
        }
    }

    /// `--addr IP` takes the default ports, and the control port becomes the
    /// advertised one once a handshake has happened.
    #[test]
    fn from_addr_uses_the_default_ports_then_adopts_the_advertised_one() {
        let d = Device::from_addr("192.0.2.9:49374".parse().unwrap());
        assert_eq!(d.frame.port(), DEFAULT_FRAME_PORT);
        assert_eq!(d.control.port(), DEFAULT_CONTROL_PORT);

        let mut d = Device::from_addr("192.0.2.9:6000".parse().unwrap());
        assert_eq!(d.control.port(), 6001, "the port above, by convention");
        let info = DeviceInfo {
            ctrl: 7777,
            ..DeviceInfo::default()
        };
        d.apply(info);
        assert_eq!(d.control.port(), 7777);
    }
}
