//! Limits on sign-in attempts and client registrations from each client address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderName};

/// Number of sign-in failures from an address before each next attempt must wait.
const FREE_SIGN_IN_FAILURES: u32 = 5;

/// Wait after the first failure past [`FREE_SIGN_IN_FAILURES`], which doubles with each further failure.
const FIRST_SIGN_IN_DELAY: Duration = Duration::from_secs(1);

/// Longest wait between two sign-in attempts from an address.
const MAX_SIGN_IN_DELAY: Duration = Duration::from_mins(15);

/// Time after the last failure at which the server forgets the failures of an address.
const SIGN_IN_FAILURE_MEMORY: Duration = Duration::from_hours(1);

/// Number of client registrations that an address can make in each [`REGISTRATION_WINDOW`].
const REGISTRATIONS_PER_WINDOW: u32 = 10;

const REGISTRATION_WINDOW: Duration = Duration::from_hours(1);

/// Number of addresses that each limit keeps state for.
const MAX_TRACKED_ADDRS: usize = 10_000;

/// Source of the client address of a request.
#[derive(Clone, Debug, Default)]
pub enum ClientAddrSource {
    /// Address of the TCP peer.
    #[default]
    Peer,
    /// Header that a trusted proxy sets to the client address, such as `Fly-Client-IP`.
    Header(HeaderName),
}

impl ClientAddrSource {
    /// Return the address of the client that sent a request from `peer` with `headers`.
    ///
    /// Returns the peer address if the header is missing or does not hold an IP address.
    pub(crate) fn client_addr(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let from_header = match self {
            Self::Peer => None,
            Self::Header(name) => headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok()),
        };
        from_header.unwrap_or_else(|| peer.ip())
    }
}

/// Return the key that groups `addr` with the other addresses of its client.
///
/// One IPv6 client usually controls a whole `/64` network.
fn addr_key(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V4(_) => addr,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
    }
}

/// Number of events from an address, and the time that the count started or last changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    n: u32,
    since: Instant,
}

/// Count of each address, for at most [`MAX_TRACKED_ADDRS`] addresses.
#[derive(Debug, Default)]
struct AddrCounts(Mutex<HashMap<IpAddr, Count>>);

impl AddrCounts {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Count>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replace the count of `addr` with the count that `f` returns, or keep it if `f` fails.
    ///
    /// `f` receives `None` if the count of `addr` started `memory` or more before `now`.
    fn update<E>(
        &self,
        addr: IpAddr,
        now: Instant,
        memory: Duration,
        f: impl FnOnce(Option<Count>) -> Result<Count, E>,
    ) -> Result<(), E> {
        let fresh = |c: &Count| now.saturating_duration_since(c.since) < memory;
        let key = addr_key(addr);
        let mut counts = self.lock();
        let next = f(counts.get(&key).copied().filter(fresh))?;
        if counts.len() >= MAX_TRACKED_ADDRS && !counts.contains_key(&key) {
            counts.retain(|_, c| fresh(c));
            if counts.len() >= MAX_TRACKED_ADDRS
                && let Some(oldest) = counts.iter().min_by_key(|(_, c)| c.since).map(|(a, _)| *a)
            {
                counts.remove(&oldest);
            }
        }
        counts.insert(key, next);
        Ok(())
    }

    fn remove(&self, addr: IpAddr) {
        self.lock().remove(&addr_key(addr));
    }
}

/// Failed sign-in attempts of each client address, which delay the next attempt from that address.
#[derive(Debug, Default)]
pub(crate) struct SignInThrottle {
    /// Number of failures of each address, with the time of the last failure.
    failures: AddrCounts,
}

impl SignInThrottle {
    /// Count an attempt from `addr` at `now` as a failure until [`Self::record_success`], or return the time that `addr` must wait first.
    pub(crate) fn try_attempt(&self, addr: IpAddr, now: Instant) -> Result<(), Duration> {
        self.failures
            .update(addr, now, SIGN_IN_FAILURE_MEMORY, |last| {
                let n = last.map_or(0, |f| f.n);
                if let Some(f) = last {
                    let ready = f.since + sign_in_delay(f.n);
                    if ready > now {
                        return Err(ready.duration_since(now));
                    }
                }
                Ok(Count {
                    n: n.saturating_add(1),
                    since: now,
                })
            })
    }

    /// Forget the failures of `addr` after a successful attempt.
    pub(crate) fn record_success(&self, addr: IpAddr) {
        self.failures.remove(addr);
    }
}

/// Return the wait after the last of `failures` sign-in failures.
fn sign_in_delay(failures: u32) -> Duration {
    let Some(doublings) = failures.checked_sub(FREE_SIGN_IN_FAILURES) else {
        return Duration::ZERO;
    };
    FIRST_SIGN_IN_DELAY
        .checked_mul(1 << doublings.min(31))
        .map_or(MAX_SIGN_IN_DELAY, |d| d.min(MAX_SIGN_IN_DELAY))
}

/// Client registrations of each client address in its current window.
#[derive(Debug, Default)]
pub(crate) struct RegistrationLimit {
    /// Number of registrations of each address, with the start of its window.
    windows: AddrCounts,
}

impl RegistrationLimit {
    /// Count a registration from `addr` at `now`, or return the time until the window of `addr` ends if the window is full.
    pub(crate) fn try_register(&self, addr: IpAddr, now: Instant) -> Result<(), Duration> {
        self.windows
            .update(addr, now, REGISTRATION_WINDOW, |window| {
                let window = window.unwrap_or(Count { n: 0, since: now });
                if window.n >= REGISTRATIONS_PER_WINDOW {
                    let elapsed = now.saturating_duration_since(window.since);
                    return Err(REGISTRATION_WINDOW.saturating_sub(elapsed));
                }
                Ok(Count {
                    n: window.n + 1,
                    ..window
                })
            })
    }
}

/// Return `wait` in whole seconds, rounded up, for a `Retry-After` header.
pub(crate) fn retry_after_secs(wait: Duration) -> u64 {
    u64::try_from(wait.as_nanos().div_ceil(1_000_000_000)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

    #[test]
    fn sign_in_waits_double_after_the_free_failures_up_to_a_limit() {
        let throttle = SignInThrottle::default();
        let t0 = Instant::now();
        for _ in 0..FREE_SIGN_IN_FAILURES {
            assert_eq!(throttle.try_attempt(ADDR, t0), Ok(()));
        }
        assert_eq!(throttle.try_attempt(ADDR, t0), Err(FIRST_SIGN_IN_DELAY));
        let t1 = t0 + FIRST_SIGN_IN_DELAY;
        assert_eq!(throttle.try_attempt(ADDR, t1), Ok(()));
        assert_eq!(throttle.try_attempt(ADDR, t1), Err(2 * FIRST_SIGN_IN_DELAY));
        assert_eq!(sign_in_delay(40), MAX_SIGN_IN_DELAY);
        assert_eq!(sign_in_delay(u32::MAX), MAX_SIGN_IN_DELAY);
    }

    #[test]
    fn sign_in_failures_end_with_a_success_or_after_the_memory() {
        let throttle = SignInThrottle::default();
        let t0 = Instant::now();
        for _ in 0..FREE_SIGN_IN_FAILURES {
            throttle.try_attempt(ADDR, t0).unwrap();
        }
        assert!(throttle.try_attempt(ADDR, t0).is_err());
        let other = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 1));
        assert_eq!(throttle.try_attempt(other, t0), Ok(()));

        let forgotten = t0 + SIGN_IN_FAILURE_MEMORY;
        for _ in 0..FREE_SIGN_IN_FAILURES {
            assert_eq!(throttle.try_attempt(ADDR, forgotten), Ok(()));
        }
        throttle.record_success(ADDR);
        assert_eq!(throttle.try_attempt(ADDR, forgotten), Ok(()));
    }

    #[test]
    fn registrations_from_an_address_are_limited_per_window() {
        let limit = RegistrationLimit::default();
        let t0 = Instant::now();
        for _ in 0..REGISTRATIONS_PER_WINDOW {
            assert_eq!(limit.try_register(ADDR, t0), Ok(()));
        }
        let half = REGISTRATION_WINDOW / 2;
        assert_eq!(limit.try_register(ADDR, t0 + half), Err(half));
        assert_eq!(limit.try_register(ADDR, t0 + REGISTRATION_WINDOW), Ok(()));
    }

    #[test]
    fn ipv6_addresses_of_one_network_share_a_limit() {
        let limit = RegistrationLimit::default();
        let t0 = Instant::now();
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        for _ in 0..REGISTRATIONS_PER_WINDOW {
            limit.try_register(a, t0).unwrap();
        }
        assert!(limit.try_register(b, t0).is_err());
        assert_eq!(limit.try_register(c, t0), Ok(()));
    }

    #[test]
    fn the_table_of_addresses_stays_bounded() {
        let counts = AddrCounts::default();
        let t0 = Instant::now();
        for i in 0..=MAX_TRACKED_ADDRS {
            let addr = IpAddr::V4(u32::try_from(i).unwrap().into());
            let count = Count { n: 1, since: t0 };
            counts
                .update(addr, t0, Duration::from_secs(1), |_| Ok::<_, ()>(count))
                .unwrap();
        }
        assert_eq!(counts.lock().len(), MAX_TRACKED_ADDRS);
    }

    #[test]
    fn a_proxy_header_names_the_client_only_if_configured() {
        let peer: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("fly-client-ip", "203.0.113.7".parse().unwrap());
        let fly = ClientAddrSource::Header(HeaderName::from_static("fly-client-ip"));
        assert_eq!(fly.client_addr(peer, &headers), ADDR);
        assert_eq!(
            ClientAddrSource::Peer.client_addr(peer, &headers),
            peer.ip()
        );
        assert_eq!(fly.client_addr(peer, &HeaderMap::new()), peer.ip());
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(2)), 2);
    }
}
