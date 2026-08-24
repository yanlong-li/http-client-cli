//! A small, dependency-free pseudo-random generator.
//!
//! Seeded from the system clock. Not cryptographically secure, which is fine
//! for the `{{$random.*}}` test-data variables it is used for.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Default for Rng {
    fn default() -> Self {
        Self::new()
    }
}

impl Rng {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        let stack = &nanos as *const u64 as u64;
        let mut state = nanos ^ stack.rotate_left(32) ^ 0xD1B54A32D192ED03;
        if state == 0 {
            state = 0x9E3779B97F4A7C15;
        }
        let mut rng = Self { state };
        rng.next_u64();
        rng
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Random integer in `[lo, hi)`.
    pub fn int_range(&mut self, lo: i64, hi: i64) -> i64 {
        assert!(hi > lo, "empty random range");
        let span = (hi - lo) as u64;
        lo + (self.next_u64() % span) as i64
    }

    pub fn uuid_v4(&mut self) -> String {
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&self.next_u64().to_le_bytes());
        bytes[8..16].copy_from_slice(&self.next_u64().to_le_bytes());
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }

    pub fn alphabetic(&mut self, length: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        (0..length)
            .map(|_| ALPHABET[(self.next_u64() as usize) % ALPHABET.len()] as char)
            .collect()
    }

    pub fn alphanumeric(&mut self, length: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
        (0..length)
            .map(|_| ALPHABET[(self.next_u64() as usize) % ALPHABET.len()] as char)
            .collect()
    }

    pub fn hexadecimal(&mut self, length: usize) -> String {
        const ALPHABET: &[u8] = b"0123456789abcdef";
        (0..length)
            .map(|_| ALPHABET[(self.next_u64() as usize) % ALPHABET.len()] as char)
            .collect()
    }

    pub fn email(&mut self) -> String {
        let user = self.alphanumeric(8).to_lowercase();
        let domains = ["example.com", "example.org", "test.dev", "example.dev"];
        let domain = domains[(self.next_u64() as usize) % domains.len()];
        format!("{user}@{domain}")
    }
}

/// Current UNIX timestamp in whole seconds.
pub fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current UTC timestamp formatted as ISO-8601 (`2026-08-24T12:34:56Z`).
pub fn iso8601_timestamp() -> String {
    let secs = unix_timestamp() as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
