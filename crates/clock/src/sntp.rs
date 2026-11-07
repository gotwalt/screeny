//! Just enough SNTP (RFC 4330) to set a clock: the request, and what a reply
//! means. The socket is the caller's.
//!
//! Times on this side are microseconds of the device's own monotonic clock, not
//! wall time. That is what makes the answer simple: the four timestamps of an
//! exchange give `wall - monotonic` directly, and that one number is the clock.

/// An SNTP packet with no extensions.
pub const PACKET: usize = 48;
/// The port servers listen on.
pub const PORT: u16 = 123;

/// Seconds from the NTP epoch (1900) to the Unix one (1970).
const UNIX_EPOCH: i64 = 2_208_988_800;

/// Why a reply was not believed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not 48 bytes or more.
    Short,
    /// Not a server's reply.
    NotServer,
    /// It does not answer the request we sent.
    WrongExchange,
    /// The server says it does not know the time (stratum 0 is a "kiss of
    /// death"; leap indicator 3 means unsynchronised).
    Unsynchronised,
    /// No timestamp in it.
    Empty,
}

/// Fill `out` with a request. `nonce` goes in the transmit timestamp, which a
/// server must echo, so a stray or spoofed datagram is not taken for the answer.
pub fn request(out: &mut [u8; PACKET], nonce: u64) {
    out.fill(0);
    out[0] = 0b00_100_011; // no leap warning, version 4, client
    out[40..48].copy_from_slice(&nonce.to_be_bytes());
}

fn timestamp(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(b)
}

/// An NTP timestamp as Unix microseconds. The 32-bit seconds wrap in 2036, so
/// anything that would read as before 2020 is taken to be in the next era.
fn unix_micros(ntp: u64) -> i64 {
    let mut seconds = (ntp >> 32) as i64;
    if seconds < UNIX_EPOCH + 1_577_836_800 {
        seconds += 1 << 32;
    }
    let micros = ((ntp & 0xffff_ffff) * 1_000_000) >> 32;
    (seconds - UNIX_EPOCH) * 1_000_000 + micros as i64
}

/// A reply to the request that carried `nonce`, sent at monotonic `sent_us` and
/// received at `got_us`: `wall - monotonic`, in microseconds.
pub fn offset(reply: &[u8], nonce: u64, sent_us: u64, got_us: u64) -> Result<i64, Refused> {
    if reply.len() < PACKET {
        return Err(Refused::Short);
    }
    if reply[0] & 0b111 != 4 {
        return Err(Refused::NotServer);
    }
    if timestamp(&reply[24..]) != nonce {
        return Err(Refused::WrongExchange);
    }
    if reply[0] >> 6 == 3 || reply[1] == 0 || reply[1] > 15 {
        return Err(Refused::Unsynchronised);
    }
    let (t2, t3) = (timestamp(&reply[32..]), timestamp(&reply[40..]));
    if t2 == 0 || t3 == 0 {
        return Err(Refused::Empty);
    }
    let (t2, t3) = (unix_micros(t2), unix_micros(t3));
    Ok(((t2 - sent_us as i64) + (t3 - got_us as i64)) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(nonce: u64, recv_unix_us: i64, send_unix_us: i64) -> [u8; PACKET] {
        let ntp = |us: i64| {
            let s = us.div_euclid(1_000_000) + UNIX_EPOCH;
            let f = (us.rem_euclid(1_000_000) as u64) << 32;
            ((s as u64) << 32) | (f / 1_000_000 + 1)
        };
        let mut p = [0u8; PACKET];
        p[0] = 0b00_100_100;
        p[1] = 2;
        p[24..32].copy_from_slice(&nonce.to_be_bytes());
        p[32..40].copy_from_slice(&ntp(recv_unix_us).to_be_bytes());
        p[40..48].copy_from_slice(&ntp(send_unix_us).to_be_bytes());
        p
    }

    const WALL: i64 = 1_790_000_000_000_000; // 2026-09-21 or so

    #[test]
    fn a_request_is_a_version_4_client_and_carries_the_nonce() {
        let mut p = [0xff; PACKET];
        request(&mut p, 0x0123_4567_89ab_cdef);
        assert_eq!(p[0] & 0b111, 3);
        assert_eq!((p[0] >> 3) & 0b111, 4);
        assert!(p[1..40].iter().all(|b| *b == 0));
        assert_eq!(&p[40..48], &[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
    }

    #[test]
    fn the_network_delay_cancels_out() {
        // Device clock is `WALL` behind the wall; 10 ms each way, 1 ms in the server.
        let (sent, got) = (5_000_000u64, 5_021_000u64);
        let r = reply(7, WALL + 5_010_000, WALL + 5_011_000);
        let got_offset = offset(&r, 7, sent, got).unwrap();
        assert!((got_offset - WALL).abs() < 3, "{}", got_offset - WALL);
    }

    #[test]
    fn replies_that_prove_nothing_are_refused() {
        let good = reply(7, WALL, WALL);
        assert_eq!(offset(&good[..47], 7, 0, 0), Err(Refused::Short));
        assert_eq!(offset(&good, 8, 0, 0), Err(Refused::WrongExchange));
        let mut kod = good;
        kod[1] = 0;
        assert_eq!(offset(&kod, 7, 0, 0), Err(Refused::Unsynchronised));
        let mut lost = good;
        lost[0] |= 0b11 << 6;
        assert_eq!(offset(&lost, 7, 0, 0), Err(Refused::Unsynchronised));
        let mut client = good;
        client[0] = 0b00_100_011;
        assert_eq!(offset(&client, 7, 0, 0), Err(Refused::NotServer));
    }

    #[test]
    fn the_2036_wrap_is_survived() {
        let after_wrap = ((1u64 << 32) + 1_000 - (1u64 << 32)) << 32;
        assert_eq!(unix_micros(after_wrap), ((1i64 << 32) + 1_000 - UNIX_EPOCH) * 1_000_000);
    }
}
