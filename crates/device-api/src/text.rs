//! The bounded text types the JSON carries, and the only ways to build them.
//!
//! Every string in this API is a `heapless::String<N>` with an `N` that comes
//! from somewhere real - [`screeny_proto::control::MAX_NAME_LEN`] for a name,
//! [`screeny_proto::control::MAX_SSID_LEN`] for an SSID - so a reply cannot be
//! longer than [`MAX_JSON_LEN`](crate::reply::StatusReply::MAX_JSON_LEN) says
//! it can.

use core::fmt::Write as _;

use heapless::String;
pub use screeny_proto::control::{MAX_NAME_LEN, MAX_PSK_LEN, MAX_SSID_LEN};

/// The stable short device id, the MAC suffix: `4a00a4` is six characters.
/// Twelve leaves room for a full MAC if a later board wants one.
pub const MAX_ID_LEN: usize = 12;

/// A firmware version string, `"0.2.0"` today. Sixteen holds a
/// `"1.10.3-rc1+abcdef"`-shaped one.
pub const MAX_FW_LEN: usize = 16;

/// A dotted-quad IPv4 address: `255.255.255.255` is fifteen characters.
pub const MAX_IP_LEN: usize = 15;

/// The short human sentence on an error reply. Long enough for "ssid is
/// longer than 32 bytes"; not a place to put a stack trace.
pub const MAX_DETAIL_LEN: usize = 48;

/// The PIN of decision 3 (parked card 041), parsed and ignored today.
pub const MAX_PIN_LEN: usize = 16;

/// The stable short device id.
pub type IdText = String<MAX_ID_LEN>;
/// A firmware version string.
pub type FwText = String<MAX_FW_LEN>;
/// The friendly name; empty means `screeny-<id>`.
pub type NameText = String<MAX_NAME_LEN>;
/// An SSID, as text. See [`ssid_text`] for what happens to one that is not
/// UTF-8.
pub type SsidText = String<MAX_SSID_LEN>;
/// A dotted-quad IPv4 address.
pub type IpText = String<MAX_IP_LEN>;
/// The short human sentence on an error reply.
pub type DetailText = String<MAX_DETAIL_LEN>;
/// The PIN of decision 3.
pub type PinText = String<MAX_PIN_LEN>;

/// Copy `s` into a bounded string, or `None` if it does not fit.
///
/// The `None` is deliberate: silently truncating a name into a reply is how
/// two views of the same device start disagreeing.
#[must_use]
pub fn text<const N: usize>(s: &str) -> Option<String<N>> {
    let mut out = String::new();
    out.push_str(s).ok()?;
    Some(out)
}

/// An SSID as JSON text, or `None` when it cannot be one.
///
/// **802.11 SSIDs are bytes, and JSON strings are text.** A non-UTF-8 SSID has
/// no honest JSON representation: lossy conversion would change its length and
/// mislead anyone comparing it with what they typed. So this returns `None`,
/// and the callers do the obvious thing with it - [`StatusReply::ssid`] and
/// [`WifiReply::ssid`] become `null`, and a scan result that cannot be named is
/// left out of [`NetworksReply`] rather than shown wrongly.
///
/// [`StatusReply::ssid`]: crate::reply::StatusReply::ssid
/// [`WifiReply::ssid`]: crate::reply::WifiReply::ssid
/// [`NetworksReply`]: crate::reply::NetworksReply
#[must_use]
pub fn ssid_text(bytes: &[u8]) -> Option<SsidText> {
    text(core::str::from_utf8(bytes).ok()?)
}

/// Format an IPv4 address the way the status page shows it.
#[must_use]
pub fn ipv4_text(octets: [u8; 4]) -> IpText {
    let mut out = IpText::new();
    // Cannot fail: "255.255.255.255" is exactly MAX_IP_LEN.
    let _ = write!(
        out,
        "{}.{}.{}.{}",
        octets[0], octets[1], octets[2], octets[3]
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_is_dotted_quad_and_always_fits() {
        assert_eq!(ipv4_text([192, 168, 7, 221]).as_str(), "192.168.7.221");
        assert_eq!(ipv4_text([255, 255, 255, 255]).as_str(), "255.255.255.255");
        assert_eq!(ipv4_text([255, 255, 255, 255]).len(), MAX_IP_LEN);
        assert_eq!(ipv4_text([0, 0, 0, 0]).as_str(), "0.0.0.0");
    }

    #[test]
    fn ssid_text_refuses_non_utf8_and_over_long() {
        assert_eq!(ssid_text(b"Example-Wifi1").unwrap().as_str(), "Example-Wifi1");
        assert_eq!(ssid_text(&[0xff, 0xfe]), None);
        assert_eq!(ssid_text(&[b'a'; MAX_SSID_LEN]).unwrap().len(), MAX_SSID_LEN);
        assert_eq!(ssid_text(&[b'a'; MAX_SSID_LEN + 1]), None);
    }

    #[test]
    fn text_refuses_what_does_not_fit() {
        assert!(text::<4>("abcd").is_some());
        assert!(text::<4>("abcde").is_none());
        // Bytes, not characters: "caf\u{e9}" is four characters but five
        // bytes, and heapless bounds by byte length.
        assert!(text::<5>("caf\u{e9}").is_some());
        assert!(text::<4>("caf\u{e9}").is_none());
    }
}
