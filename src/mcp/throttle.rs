//! Limits on incorrect passwords, rejected bearer tokens, client registrations, and OAuth requests from each client address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, HeaderName};
use chrono::{DateTime, Duration, Utc};

use crate::Result;
use crate::db::{Db, Txn, run_txn};
use crate::schema::OAuthAddrCount;

/// Number of incorrect passwords from an address before each next sign-in must wait.
const FREE_FAILURES: u32 = 5;

/// Wait after the first failure past [`FREE_FAILURES`], which doubles with each further failure.
const FIRST_FAILURE_DELAY: Duration = Duration::seconds(1);

/// Longest wait between two sign-in attempts from an address.
const MAX_FAILURE_DELAY: Duration = Duration::minutes(15);

/// Time after the last incorrect password at which the server forgets the failures of an address.
const FAILURE_MEMORY: Duration = Duration::hours(1);

/// Limit on client registrations from each address.
const REGISTRATIONS: Window = Window {
    per_window: 10,
    window: Duration::hours(1),
};

/// Limit on rejected bearer tokens from each address, which bounds the database reads of token checks.
const BEARER_FAILURES: Window = Window {
    per_window: 20,
    window: Duration::minutes(1),
};

/// Limit on requests from each address to the OAuth routes that read the database.
const REQUESTS: Window = Window {
    per_window: 60,
    window: Duration::minutes(1),
};

/// Number of addresses that each limit keeps in memory.
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
/// A single IPv6 client usually controls a `/64` network.
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
    since: DateTime<Utc>,
}

/// Rule of a limit on the events of each client address.
trait Rule: Copy {
    /// Return the time after the start or last change of a count at which the count ends.
    fn memory(self) -> Duration;

    /// Return the count after an event at `now` that follows the current count `last`, or the time that the event must wait.
    fn next(self, last: Option<Count>, now: DateTime<Utc>) -> std::result::Result<Count, Duration>;

    /// Return `count` if it has not ended at `now`.
    fn current(self, count: Option<Count>, now: DateTime<Utc>) -> Option<Count> {
        count.filter(|c| now - c.since < self.memory())
    }
}

/// Rule that delays each attempt after [`FREE_FAILURES`] failures, with a wait that doubles after each further failure.
#[derive(Clone, Copy, Debug)]
struct Backoff;

impl Rule for Backoff {
    fn memory(self) -> Duration {
        FAILURE_MEMORY
    }

    fn next(self, last: Option<Count>, now: DateTime<Utc>) -> std::result::Result<Count, Duration> {
        if let Some(last) = last {
            let ready = last.since + failure_delay(last.n);
            if ready > now {
                return Err(ready - now);
            }
        }
        Ok(Count {
            n: last.map_or(0, |c| c.n).saturating_add(1),
            since: now,
        })
    }
}

/// Rule that allows `per_window` events in each `window` that the first event starts.
#[derive(Clone, Copy, Debug)]
struct Window {
    per_window: u32,
    window: Duration,
}

impl Rule for Window {
    fn memory(self) -> Duration {
        self.window
    }

    fn next(self, last: Option<Count>, now: DateTime<Utc>) -> std::result::Result<Count, Duration> {
        let window = last.unwrap_or(Count { n: 0, since: now });
        if window.n >= self.per_window {
            return Err(self.window - (now - window.since));
        }
        Ok(Count {
            n: window.n + 1,
            ..window
        })
    }
}

/// Return the wait after the last of `failures` failed credentials.
fn failure_delay(failures: u32) -> Duration {
    let Some(doublings) = failures.checked_sub(FREE_FAILURES) else {
        return Duration::zero();
    };
    FIRST_FAILURE_DELAY
        .checked_mul(1 << doublings.min(20))
        .map_or(MAX_FAILURE_DELAY, |d| d.min(MAX_FAILURE_DELAY))
}

/// Limit of rule `R` on each client address, counted only in the memory of this server, for at most [`MAX_TRACKED_ADDRS`] addresses.
struct MemoryLimit<R> {
    rule: R,
    counts: Mutex<HashMap<IpAddr, Count>>,
}

impl<R: Rule> MemoryLimit<R> {
    fn new(rule: R) -> Self {
        Self {
            rule,
            counts: Mutex::default(),
        }
    }

    /// Count an event from `addr` at `now`, or return the time that the event must wait.
    fn try_count(&self, addr: IpAddr, now: DateTime<Utc>) -> std::result::Result<(), Duration> {
        let key = addr_key(addr);
        let mut counts = lock(&self.counts);
        let next = self
            .rule
            .next(self.rule.current(counts.get(&key).copied(), now), now)?;
        if counts.len() >= MAX_TRACKED_ADDRS && !counts.contains_key(&key) {
            counts.retain(|_, c| self.rule.current(Some(*c), now).is_some());
            if counts.len() >= MAX_TRACKED_ADDRS
                && let Some(oldest) = counts.iter().min_by_key(|(_, c)| c.since).map(|(a, _)| *a)
            {
                counts.remove(&oldest);
            }
        }
        counts.insert(key, next);
        Ok(())
    }

    /// Return the time that an event from `addr` at `now` must wait, without a count.
    fn wait(&self, addr: IpAddr, now: DateTime<Utc>) -> Option<Duration> {
        let last = lock(&self.counts).get(&addr_key(addr)).copied();
        self.rule.next(self.rule.current(last, now), now).err()
    }

    /// Forget the count of `addr`.
    fn forget(&self, addr: IpAddr) {
        lock(&self.counts).remove(&addr_key(addr));
    }
}

/// Limit of rule `R` on each client address, counted in memory and in the database.
///
/// The memory count stops a burst of requests before it reaches the database. The stored count keeps the limit across restarts and machines.
struct StoredLimit<R> {
    /// Name of the limit in the stored key of a count.
    name: &'static str,
    memory: MemoryLimit<R>,
    /// End of the last wait that the stored count of each address returned, for at most [`MAX_TRACKED_ADDRS`] addresses.
    stored_waits: Mutex<HashMap<IpAddr, DateTime<Utc>>>,
    db: Db,
}

impl<R: Rule> StoredLimit<R> {
    fn new(name: &'static str, rule: R, db: Db) -> Self {
        Self {
            name,
            memory: MemoryLimit::new(rule),
            stored_waits: Mutex::default(),
            db,
        }
    }

    /// Count an event from `addr` at `now`, or return the time that the event must wait.
    async fn try_count(&self, addr: IpAddr, now: DateTime<Utc>) -> Result<Option<Duration>> {
        let key = addr_key(addr);
        if let Some(end) = lock(&self.stored_waits).get(&key).filter(|end| **end > now) {
            return Ok(Some(*end - now));
        }
        if let Err(wait) = self.memory.try_count(addr, now) {
            return Ok(Some(wait));
        }
        let stored_key = self.stored_key(key);
        let wait = run_txn!(&self.db, |txn| self
            .count_stored(&mut txn, &stored_key, now)
            .await)?;
        if let Some(wait) = wait {
            let mut waits = lock(&self.stored_waits);
            if waits.len() >= MAX_TRACKED_ADDRS {
                waits.retain(|_, end| *end > now);
            }
            if waits.len() < MAX_TRACKED_ADDRS {
                waits.insert(key, now + wait);
            }
        }
        Ok(wait)
    }

    /// Forget the count of `addr`.
    async fn reset(&self, addr: IpAddr) -> Result<()> {
        let key = addr_key(addr);
        self.memory.forget(addr);
        lock(&self.stored_waits).remove(&key);
        self.db
            .delete_oauth::<OAuthAddrCount>(&self.stored_key(key))
            .await
    }

    /// Return the key of the stored count of address key `key`.
    fn stored_key(&self, key: IpAddr) -> String {
        format!("{}/{key}", self.name)
    }

    /// Count an event in the stored count `stored_key`, or return the time that the event must wait.
    async fn count_stored(
        &self,
        txn: &mut Txn,
        stored_key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Duration>> {
        let rule = self.memory.rule;
        let last = txn
            .get_oauth::<OAuthAddrCount>(stored_key)
            .await?
            .map(|row| Count {
                n: row.n,
                since: row.since,
            });
        let next = match rule.next(rule.current(last, now), now) {
            Ok(next) => next,
            Err(wait) => return Ok(Some(wait)),
        };
        let ttl = (next.since + rule.memory() - now)
            .to_std()
            .unwrap_or_default();
        let row = OAuthAddrCount {
            key: stored_key.to_string(),
            n: next.n,
            since: next.since,
        };
        txn.delete_oauth::<OAuthAddrCount>(stored_key);
        txn.insert_oauth(&row, ttl);
        Ok(None)
    }
}

/// Lock `mutex`, and use its state even if a thread panicked while it held the lock.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Limits on incorrect passwords, rejected bearer tokens, client registrations, and OAuth requests from each client address.
#[derive(Clone)]
pub(crate) struct AddrLimits {
    client_addrs: ClientAddrSource,
    password_failures: Arc<StoredLimit<Backoff>>,
    bearer_failures: Arc<MemoryLimit<Window>>,
    registrations: Arc<StoredLimit<Window>>,
    requests: Arc<MemoryLimit<Window>>,
}

impl AddrLimits {
    /// Create the limits for the client addresses from `client_addrs`, with counts stored in `db`.
    pub(crate) fn new(db: &Db, client_addrs: ClientAddrSource) -> Self {
        Self {
            client_addrs,
            password_failures: Arc::new(StoredLimit::new("password_failures", Backoff, db.clone())),
            bearer_failures: Arc::new(MemoryLimit::new(BEARER_FAILURES)),
            registrations: Arc::new(StoredLimit::new("registrations", REGISTRATIONS, db.clone())),
            requests: Arc::new(MemoryLimit::new(REQUESTS)),
        }
    }

    /// Return the address of the client that sent a request from `peer` with `headers`.
    pub(crate) fn client_addr(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        self.client_addrs.client_addr(peer, headers)
    }

    /// Count a sign-in from `addr` as an incorrect password until [`Self::record_password_success`], or return the time that `addr` must wait first.
    pub(crate) async fn try_password(&self, addr: IpAddr) -> Result<Option<Duration>> {
        self.password_failures.try_count(addr, Utc::now()).await
    }

    /// Forget the incorrect passwords of `addr` after a valid password.
    pub(crate) async fn record_password_success(&self, addr: IpAddr) -> Result<()> {
        self.password_failures.reset(addr).await
    }

    /// Return the time that the next bearer token from `addr` must wait after too many rejected tokens.
    pub(crate) fn bearer_wait(&self, addr: IpAddr) -> Option<Duration> {
        self.bearer_failures.wait(addr, Utc::now())
    }

    /// Count a rejected bearer token from `addr`.
    pub(crate) fn record_bearer_failure(&self, addr: IpAddr) {
        let _full = self.bearer_failures.try_count(addr, Utc::now());
    }

    /// Count a request from `addr` to an OAuth route that reads the database, only in memory, or return the time until the window of `addr` ends if the window is full.
    pub(crate) fn try_request(&self, addr: IpAddr) -> Option<Duration> {
        self.requests.try_count(addr, Utc::now()).err()
    }

    /// Count a client registration from `addr`, or return the time until the window of `addr` ends if the window is full.
    pub(crate) async fn try_register(&self, addr: IpAddr) -> Result<Option<Duration>> {
        self.registrations.try_count(addr, Utc::now()).await
    }
}

/// Return `wait` in whole seconds, rounded up, for a `Retry-After` header.
pub(crate) fn retry_after_secs(wait: Duration) -> u64 {
    u64::try_from(wait.num_milliseconds())
        .unwrap_or(0)
        .div_ceil(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

    /// Count `n` events of `rule` at `now`, starting from `last`, and return the final count.
    fn count_n(
        rule: impl Rule,
        mut last: Option<Count>,
        n: u32,
        now: DateTime<Utc>,
    ) -> Option<Count> {
        for _ in 0..n {
            last = Some(rule.next(rule.current(last, now), now).unwrap());
        }
        last
    }

    #[test]
    fn failure_waits_double_after_the_free_failures_up_to_a_limit() {
        let rule = Backoff;
        let t0 = Utc::now();
        let last = count_n(rule, None, FREE_FAILURES, t0);
        assert_eq!(rule.next(last, t0), Err(FIRST_FAILURE_DELAY));
        let t1 = t0 + FIRST_FAILURE_DELAY;
        let last = Some(rule.next(last, t1).unwrap());
        assert_eq!(rule.next(last, t1), Err(FIRST_FAILURE_DELAY * 2));
        assert_eq!(failure_delay(40), MAX_FAILURE_DELAY);
        assert_eq!(failure_delay(u32::MAX), MAX_FAILURE_DELAY);
    }

    #[test]
    fn failures_end_after_the_memory() {
        let rule = Backoff;
        let t0 = Utc::now();
        let last = count_n(rule, None, FREE_FAILURES, t0);
        let forgotten = t0 + FAILURE_MEMORY;
        assert_eq!(rule.current(last, forgotten), None);
        assert!(
            rule.current(last, forgotten - Duration::seconds(1))
                .is_some()
        );
    }

    #[test]
    fn registrations_from_an_address_are_limited_per_window() {
        let rule = REGISTRATIONS;
        let t0 = Utc::now();
        let last = count_n(rule, None, rule.per_window, t0);
        let half = rule.window / 2;
        assert_eq!(
            rule.next(rule.current(last, t0 + half), t0 + half),
            Err(half)
        );
        let next = t0 + rule.window;
        assert!(rule.next(rule.current(last, next), next).is_ok());
    }

    #[test]
    fn requests_from_an_address_are_limited_per_minute() {
        let rule = REQUESTS;
        let t0 = Utc::now();
        let last = count_n(rule, None, rule.per_window, t0);
        assert_eq!(rule.next(last, t0), Err(rule.window));
        let next = t0 + rule.window;
        assert!(rule.next(rule.current(last, next), next).is_ok());
    }

    #[test]
    fn ipv6_addresses_in_a_slash_64_network_share_a_key() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(addr_key(a), addr_key(b));
        assert_ne!(addr_key(a), addr_key(c));
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        assert_eq!(addr_key(mapped), ADDR);
    }

    #[test]
    fn the_memory_table_of_addresses_stays_bounded() {
        let limit = MemoryLimit::new(REGISTRATIONS);
        let t0 = Utc::now();
        for i in 0..=MAX_TRACKED_ADDRS {
            let addr = IpAddr::V4(u32::try_from(i).unwrap().into());
            limit.try_count(addr, t0).unwrap();
        }
        assert_eq!(lock(&limit.counts).len(), MAX_TRACKED_ADDRS);
    }

    #[test]
    fn client_addr_uses_a_proxy_header_only_if_configured() {
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
        assert_eq!(retry_after_secs(Duration::milliseconds(1)), 1);
        assert_eq!(retry_after_secs(Duration::seconds(2)), 2);
    }
}
