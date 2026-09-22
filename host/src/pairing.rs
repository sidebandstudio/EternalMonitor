//! Pairing credentials and bounded, clock-injected failed-attempt accounting.
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{Hello2, HelloStatus};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use crate::transport::link::{LinkId, PeerId};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_IPS: usize = 64;

pub fn new_token() -> Result<[u8; 16], getrandom::Error> {
    loop {
        let mut token = [0; 16];
        getrandom::fill(&mut token)?;
        if token != [0; 16] {
            return Ok(token);
        }
    }
}

pub fn token_hex(token: &[u8; 16]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn parse_token(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut token = [0; 16];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    (token != [0; 16]).then_some(token)
}

fn random_code(previous: u32) -> Result<u32, getrandom::Error> {
    // Rejection sampling avoids modulo bias. Exclude 000000 (wire sentinel)
    // and the preceding code so a rotation always changes what the user sees.
    loop {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes)?;
        let value = u32::from_ne_bytes(bytes);
        if value < u32::MAX - u32::MAX % 999_999 {
            let code = value % 999_999 + 1;
            if code != previous {
                return Ok(code);
            }
        }
    }
}

pub struct Pairing {
    pub required: bool,
    token: [u8; 16],
    code: u32,
    limiter: RateLimiter,
}

pub struct Grant {
    pub token: [u8; 16],
    pub used_code: bool,
}

impl Pairing {
    pub fn new(required: bool, token: [u8; 16]) -> Result<Self, getrandom::Error> {
        assert_ne!(token, [0; 16]);
        Ok(Self {
            required,
            token,
            code: random_code(0)?,
            limiter: RateLimiter::default(),
        })
    }

    pub fn configure(&mut self, required: bool, token: [u8; 16]) {
        assert_ne!(token, [0; 16]);
        self.required = required;
        self.token = token;
    }

    pub fn token(&self) -> [u8; 16] {
        self.token
    }
    pub fn code(&self) -> u32 {
        self.code
    }
    pub fn ack_token(&self) -> [u8; 16] {
        if self.required {
            self.token
        } else {
            [0; 16]
        }
    }

    pub fn log_code(&self) {
        info!(pairing_code = %format_args!("{:06}", self.code), "Pairing code ready");
    }

    pub fn rotate_code(&mut self) -> Result<(), getrandom::Error> {
        self.code = random_code(self.code)?;
        self.log_code();
        Ok(())
    }

    pub fn regenerate_token(&mut self) -> Result<(), getrandom::Error> {
        let token = new_token()?;
        self.rotate_code()?;
        self.token = token;
        Ok(())
    }

    pub fn authorize(
        &mut self,
        peer: PeerId,
        hello: &Hello2,
        now: Instant,
    ) -> Result<Grant, HelloStatus> {
        if !self.required {
            return Ok(Grant {
                token: [0; 16],
                used_code: false,
            });
        }
        if bool::from(hello.auth_token.ct_eq(&self.token)) {
            return Ok(Grant {
                token: self.token,
                used_code: false,
            });
        }
        let usb = matches!(peer.link, LinkId::Usb { .. });
        if !usb
            && peer
                .addr
                .is_some_and(|addr| self.limiter.blocked(addr.ip(), now))
        {
            return Err(HelloStatus::RateLimited);
        }
        if hello.pairing_code != 0 && bool::from(hello.pairing_code.ct_eq(&self.code)) {
            return Ok(Grant {
                token: self.token,
                used_code: true,
            });
        }
        if usb {
            return Ok(Grant {
                token: self.token,
                used_code: false,
            });
        }
        if let Some(addr) = peer.addr {
            if self.limiter.fail(addr.ip(), now) {
                warn!(ip = %addr.ip(), "Pairing rate limited for 60 seconds");
                return Err(HelloStatus::RateLimited);
            }
        }
        Err(HelloStatus::Unauthorized)
    }
}

#[derive(Default)]
struct RateLimiter {
    entries: HashMap<IpAddr, Attempts>,
    // Untracked addresses share a bucket when full. Never evict a live ban:
    // cycling spoofed IPs must not reset another address's failed attempts.
    overflow: Option<Attempts>,
}

struct Attempts {
    since: Instant,
    failures: u8,
    blocked_until: Option<Instant>,
}

impl Attempts {
    fn active(&self, now: Instant) -> bool {
        self.blocked_until
            .map_or(now < self.since + WINDOW, |until| now < until)
    }
    fn blocked(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|until| now < until)
    }
}

impl RateLimiter {
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| entry.active(now));
        if self
            .overflow
            .as_ref()
            .is_some_and(|entry| !entry.active(now))
        {
            self.overflow = None;
        }
    }
    fn blocked(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.prune(now);
        self.entries
            .get(&ip)
            .or(self.overflow.as_ref())
            .is_some_and(|entry| entry.blocked(now))
    }
    /// True only when this failure starts a ban (one warning per ban).
    fn fail(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.prune(now);
        let fresh = || Attempts {
            since: now,
            failures: 0,
            blocked_until: None,
        };
        let entry = if self.entries.contains_key(&ip) || self.entries.len() < MAX_IPS {
            self.entries.entry(ip).or_insert_with(fresh)
        } else {
            self.overflow.get_or_insert_with(fresh)
        };
        if entry.blocked(now) {
            return false;
        }
        entry.failures += 1;
        if entry.failures == 5 {
            entry.blocked_until = Some(now + WINDOW);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_parser_and_generated_credentials() {
        let token = new_token().unwrap();
        assert_eq!(parse_token(&token_hex(&token)), Some(token));
        assert_eq!(parse_token(&token_hex(&token).to_uppercase()), Some(token));
        for bad in [
            "",
            "00",
            "00000000000000000000000000000000",
            "gggggggggggggggggggggggggggggggg",
            "éééééééééééééééé",
        ] {
            assert!(parse_token(bad).is_none());
        }
        let mut pairing = Pairing::new(true, token).unwrap();
        let code = pairing.code();
        assert!((1..=999_999).contains(&code));
        pairing.regenerate_token().unwrap();
        assert_ne!(pairing.token(), token);
        assert_ne!(pairing.code(), code);
    }

    #[test]
    fn failures_are_per_ip_and_bans_expire_without_extension() {
        let mut limiter = RateLimiter::default();
        let ip = "192.0.2.1".parse().unwrap();
        let other = "192.0.2.2".parse().unwrap();
        let start = Instant::now();
        for second in 0..4 {
            assert!(!limiter.fail(ip, start + Duration::from_secs(second)));
        }
        assert!(!limiter.blocked(ip, start + Duration::from_secs(4)));
        assert!(limiter.fail(ip, start + Duration::from_secs(4)));
        assert!(!limiter.blocked(other, start + Duration::from_secs(5)));
        assert!(limiter.blocked(ip, start + Duration::from_secs(63)));
        assert!(!limiter.fail(ip, start + Duration::from_secs(63)));
        assert!(!limiter.blocked(ip, start + Duration::from_secs(64)));
        for _ in 0..4 {
            assert!(!limiter.fail(ip, start + Duration::from_secs(64)));
        }
        assert!(limiter.fail(ip, start + Duration::from_secs(64)));
    }

    #[test]
    fn unexpired_limits_survive_address_flood_and_partial_windows_expire() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let ip = "192.0.2.1".parse().unwrap();
        for _ in 0..5 {
            limiter.fail(ip, start);
        }
        for n in 0..1024 {
            limiter.fail(IpAddr::V6(std::net::Ipv6Addr::from(n)), start);
        }
        assert_eq!(limiter.entries.len(), MAX_IPS);
        assert!(limiter.blocked(ip, start));
        assert!(limiter.blocked("198.51.100.1".parse().unwrap(), start));
        let later = start + WINDOW;
        assert!(!limiter.blocked(ip, later));
        assert_eq!(limiter.entries.len(), 0);
        for _ in 0..4 {
            assert!(!limiter.fail(ip, later));
        }
        assert!(!limiter.fail(ip, later + WINDOW));
    }
}
