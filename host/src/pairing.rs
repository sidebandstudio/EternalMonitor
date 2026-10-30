//! Pairing credentials and bounded, clock-injected failed-attempt accounting.
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{Hello2, HelloStatus};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use crate::transport::link::{LinkId, PeerId};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_IPS: usize = 64;
/// Wrong codes the host takes, from every address together, per
/// [`BUDGET_WINDOW`]. One more pauses code pairing for the window.
pub const CODE_BUDGET: usize = 10;
pub const BUDGET_WINDOW: Duration = Duration::from_secs(600);

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
    budget: CodeBudget,
}

pub struct Grant {
    pub token: [u8; 16],
    pub used_code: bool,
    /// The iPad paired just now, with the code or over USB, and gets the
    /// token in the ACK. An iPad that presented the token already has it.
    pub new_pairing: bool,
}

impl Pairing {
    pub fn new(required: bool, token: [u8; 16]) -> Result<Self, getrandom::Error> {
        assert_ne!(token, [0; 16]);
        Ok(Self {
            required,
            token,
            code: random_code(0)?,
            limiter: RateLimiter::default(),
            budget: CodeBudget::default(),
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

    /// Said at startup and whenever pairing is turned off.
    pub fn warn_if_off(&self) {
        if !self.required {
            warn!("Pairing is off: any iPad on the network can connect without a code");
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

    /// How much longer code pairing stays paused after too many wrong
    /// codes. QR codes, USB and paired iPads work meanwhile.
    pub fn code_pairing_paused(&self, now: Instant) -> Option<Duration> {
        self.budget
            .paused_until
            .and_then(|until| until.checked_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    /// Resumes code pairing from the PC, with a full budget, no address
    /// limits and a new code.
    pub fn resume_code_pairing(&mut self) -> Result<(), getrandom::Error> {
        self.rotate_code()?;
        self.budget = CodeBudget::default();
        self.limiter = RateLimiter::default();
        info!("Code pairing resumed from the PC");
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
                new_pairing: false,
            });
        }
        if bool::from(hello.auth_token.ct_eq(&self.token)) {
            return Ok(Grant {
                token: self.token,
                used_code: false,
                new_pairing: false,
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
        if !usb && hello.pairing_code != 0 && self.budget.paused(now) {
            return Err(HelloStatus::RateLimited);
        }
        if hello.pairing_code != 0 && bool::from(hello.pairing_code.ct_eq(&self.code)) {
            return Ok(Grant {
                token: self.token,
                used_code: true,
                new_pairing: true,
            });
        }
        if usb {
            return Ok(Grant {
                token: self.token,
                used_code: false,
                new_pairing: true,
            });
        }
        let mut limited = false;
        if let Some(addr) = peer.addr {
            if self
                .limiter
                .fail(addr.ip(), FailedRequest::from(hello), now)
            {
                warn!(ip = %addr.ip(), "Pairing rate limited for 60 seconds");
                limited = true;
            }
        }
        if hello.pairing_code != 0 && self.budget.spend(FailedRequest::from(hello), now) {
            warn!(
                wrong_codes = CODE_BUDGET,
                minutes = BUDGET_WINDOW.as_secs() / 60,
                "Too many wrong pairing codes: code pairing paused, with a new code"
            );
            if let Err(error) = self.rotate_code() {
                warn!(%error, "Pairing code rotation failed");
            }
            limited = true;
        }
        if limited {
            return Err(HelloStatus::RateLimited);
        }
        Err(HelloStatus::Unauthorized)
    }
}

/// Wrong codes from every address together, beside the limit per address.
#[derive(Default)]
struct CodeBudget {
    /// Wrong codes in the last [`BUDGET_WINDOW`], oldest first.
    spent: VecDeque<(Instant, FailedRequest)>,
    paused_until: Option<Instant>,
}

impl CodeBudget {
    fn paused(&mut self, now: Instant) -> bool {
        if self.paused_until.is_some_and(|until| now >= until) {
            *self = Self::default();
        }
        self.paused_until.is_some()
    }

    /// Counts one wrong code. True when it spends the budget and pauses
    /// code pairing.
    fn spend(&mut self, request: FailedRequest, now: Instant) -> bool {
        if self.paused(now) {
            return false;
        }
        while self
            .spent
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= BUDGET_WINDOW)
        {
            self.spent.pop_front();
        }
        // A retransmitted HELLO is the same guess.
        if self.spent.iter().any(|(_, spent)| *spent == request) {
            return false;
        }
        self.spent.push_back((now, request));
        if self.spent.len() < CODE_BUDGET {
            return false;
        }
        self.paused_until = Some(now + BUDGET_WINDOW);
        true
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
    failures: Vec<FailedRequest>,
    blocked_until: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FailedRequest {
    nonce: u32,
    code: u32,
    token: [u8; 16],
}

impl From<&Hello2> for FailedRequest {
    fn from(hello: &Hello2) -> Self {
        Self {
            nonce: hello.client_nonce,
            code: hello.pairing_code,
            token: hello.auth_token,
        }
    }
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
    fn fail(&mut self, ip: IpAddr, request: FailedRequest, now: Instant) -> bool {
        self.prune(now);
        let fresh = || Attempts {
            since: now,
            failures: Vec::with_capacity(5),
            blocked_until: None,
        };
        let entry = if self.entries.contains_key(&ip) || self.entries.len() < MAX_IPS {
            self.entries.entry(ip).or_insert_with(fresh)
        } else {
            self.overflow.get_or_insert_with(fresh)
        };
        if entry.blocked(now) || entry.failures.contains(&request) {
            return false;
        }
        // HELLO retransmissions retain their nonce and credentials. Count
        // each guess once, even when its rejection ACK was lost or delayed.
        // Changing credentials with the same nonce remains a new guess.
        entry.failures.push(request);
        if entry.failures.len() == 5 {
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

    fn guess(nonce: u32) -> FailedRequest {
        FailedRequest {
            nonce,
            code: 0,
            token: [0; 16],
        }
    }

    fn hello(nonce: u32, token: [u8; 16], code: u32) -> Hello2 {
        Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port: 50000,
            decoder_caps: 0,
            feature_caps: 0,
            screen_px_w: 0,
            screen_px_h: 0,
            screen_pt_w: 0,
            screen_pt_h: 0,
            refresh_hz: 60,
            device_name: String::new(),
            device_id: 1,
            preferred_fps: 0,
            auth_token: token,
            pairing_code: code,
        }
    }

    /// A different address for every guess, so the per-address limit never
    /// applies.
    fn wifi(n: u32) -> PeerId {
        PeerId::udp(std::net::SocketAddr::new(
            IpAddr::V4(std::net::Ipv4Addr::from(0xC000_0200 + n)),
            50000,
        ))
    }

    fn wrong_code(pairing: &Pairing) -> u32 {
        pairing.code() % 999_999 + 1
    }

    #[test]
    fn wrong_codes_from_any_address_pause_code_pairing_with_a_new_code() {
        let token = [7; 16];
        let mut pairing = Pairing::new(true, token).unwrap();
        let start = Instant::now();
        let first_code = pairing.code();
        for n in 1..CODE_BUDGET as u32 {
            let wrong = wrong_code(&pairing);
            let status = pairing.authorize(wifi(n), &hello(n, [0; 16], wrong), start);
            assert_eq!(status.err(), Some(HelloStatus::Unauthorized));
            // A retransmit is the same guess, and HELLOs without a code
            // aren't guesses.
            let again = pairing.authorize(wifi(n), &hello(n, [0; 16], wrong), start);
            assert_eq!(again.err(), Some(HelloStatus::Unauthorized));
            let no_code = pairing.authorize(wifi(n), &hello(n + 100, [0; 16], 0), start);
            assert_eq!(no_code.err(), Some(HelloStatus::Unauthorized));
        }
        assert_eq!(pairing.code(), first_code);
        assert_eq!(pairing.code_pairing_paused(start), None);
        let last = pairing.authorize(wifi(99), &hello(99, [0; 16], wrong_code(&pairing)), start);
        assert_eq!(last.err(), Some(HelloStatus::RateLimited));
        assert_ne!(pairing.code(), first_code);
        assert_eq!(pairing.code_pairing_paused(start), Some(BUDGET_WINDOW));

        // Paused: even the right code is turned away. Paired iPads, a QR
        // token and USB still connect, and a HELLO without a code still
        // asks for one.
        let later = start + BUDGET_WINDOW - Duration::from_secs(1);
        let code = pairing.code();
        let right = pairing.authorize(wifi(200), &hello(200, [0; 16], code), later);
        assert_eq!(right.err(), Some(HelloStatus::RateLimited));
        assert!(pairing
            .authorize(wifi(201), &hello(201, token, 0), later)
            .is_ok());
        let usb = pairing.authorize(PeerId::usb(1), &hello(202, [0; 16], 0), later);
        assert!(usb.is_ok_and(|grant| grant.new_pairing));
        let ask = pairing.authorize(wifi(203), &hello(203, [0; 16], 0), later);
        assert_eq!(ask.err(), Some(HelloStatus::Unauthorized));
        assert_eq!(pairing.code(), code);

        // The window refills the whole budget, and the new code works.
        let refilled = start + BUDGET_WINDOW;
        assert_eq!(pairing.code_pairing_paused(refilled), None);
        let grant = pairing.authorize(wifi(300), &hello(300, [0; 16], code), refilled);
        assert!(grant.is_ok_and(|grant| grant.used_code && grant.new_pairing));
    }

    #[test]
    fn resuming_from_the_pc_refills_the_budget_with_a_new_code() {
        let mut pairing = Pairing::new(true, [7; 16]).unwrap();
        let now = Instant::now();
        for n in 0..CODE_BUDGET as u32 {
            let _ = pairing.authorize(wifi(n), &hello(n, [0; 16], wrong_code(&pairing)), now);
        }
        assert!(pairing.code_pairing_paused(now).is_some());
        let paused_code = pairing.code();
        pairing.resume_code_pairing().unwrap();
        assert_eq!(pairing.code_pairing_paused(now), None);
        assert_ne!(pairing.code(), paused_code);
        for n in 0..CODE_BUDGET as u32 - 1 {
            let wrong = wrong_code(&pairing);
            let status = pairing.authorize(wifi(n), &hello(100 + n, [0; 16], wrong), now);
            assert_eq!(status.err(), Some(HelloStatus::Unauthorized));
        }
        let code = pairing.code();
        assert!(pairing
            .authorize(wifi(500), &hello(500, [0; 16], code), now)
            .is_ok());
    }

    #[test]
    fn resuming_from_the_pc_lifts_address_limits_so_the_new_code_pairs_at_once() {
        let mut pairing = Pairing::new(true, [7; 16]).unwrap();
        let now = Instant::now();
        let user = wifi(1);
        for n in 0..5 {
            let _ = pairing.authorize(user, &hello(n, [0; 16], wrong_code(&pairing)), now);
        }
        for n in 5..10 {
            let _ = pairing.authorize(wifi(n), &hello(n, [0; 16], wrong_code(&pairing)), now);
        }
        assert!(pairing.code_pairing_paused(now).is_some());
        pairing.resume_code_pairing().unwrap();
        let code = pairing.code();
        let grant = pairing.authorize(user, &hello(100, [0; 16], code), now);
        assert!(grant.is_ok_and(|grant| grant.used_code && grant.new_pairing));
    }

    #[test]
    fn wrong_codes_older_than_the_window_leave_the_budget() {
        let mut pairing = Pairing::new(true, [7; 16]).unwrap();
        let start = Instant::now();
        let guess = |pairing: &mut Pairing, n: u32, at: Instant| {
            let wrong = wrong_code(pairing);
            pairing.authorize(wifi(n), &hello(n, [0; 16], wrong), at)
        };
        for n in 0..CODE_BUDGET as u32 - 1 {
            assert!(guess(&mut pairing, n, start).is_err());
        }
        // A window later the first nine have aged out.
        let later = start + BUDGET_WINDOW;
        for n in 0..CODE_BUDGET as u32 - 1 {
            assert_eq!(
                guess(&mut pairing, 100 + n, later).err(),
                Some(HelloStatus::Unauthorized)
            );
        }
        assert_eq!(pairing.code_pairing_paused(later), None);
        assert_eq!(
            guess(&mut pairing, 200, later).err(),
            Some(HelloStatus::RateLimited)
        );
    }

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
            assert!(!limiter.fail(
                ip,
                guess(second as u32),
                start + Duration::from_secs(second)
            ));
        }
        assert!(!limiter.blocked(ip, start + Duration::from_secs(4)));
        assert!(limiter.fail(ip, guess(4), start + Duration::from_secs(4)));
        assert!(!limiter.blocked(other, start + Duration::from_secs(5)));
        assert!(limiter.blocked(ip, start + Duration::from_secs(63)));
        assert!(!limiter.fail(ip, guess(5), start + Duration::from_secs(63)));
        assert!(!limiter.blocked(ip, start + Duration::from_secs(64)));
        for n in 0..4 {
            assert!(!limiter.fail(ip, guess(n), start + Duration::from_secs(64)));
        }
        assert!(limiter.fail(ip, guess(4), start + Duration::from_secs(64)));
    }

    #[test]
    fn unexpired_limits_survive_address_flood_and_partial_windows_expire() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let ip = "192.0.2.1".parse().unwrap();
        for n in 0..5 {
            limiter.fail(ip, guess(n), start);
        }
        for n in 0..1024 {
            limiter.fail(
                IpAddr::V6(std::net::Ipv6Addr::from(n)),
                guess(n as u32),
                start,
            );
        }
        assert_eq!(limiter.entries.len(), MAX_IPS);
        assert!(limiter.blocked(ip, start));
        assert!(limiter.blocked("198.51.100.1".parse().unwrap(), start));
        let later = start + WINDOW;
        assert!(!limiter.blocked(ip, later));
        assert_eq!(limiter.entries.len(), 0);
        for n in 0..4 {
            assert!(!limiter.fail(ip, guess(n), later));
        }
        assert!(!limiter.fail(ip, guess(4), later + WINDOW));
    }
}
