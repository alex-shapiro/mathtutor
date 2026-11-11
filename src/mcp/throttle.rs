//! Limits on failed credentials, client registrations, and OAuth requests from each client address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, HeaderName};
use chrono::{DateTime, Duration, Utc};

use crate::Result;
use crate::db::{Db, Txn, run_txn};
use crate::schema::OAuthAddrCount;

/// Number of failed credentials from an address before each next attempt must wait.
const FREE_FAILURES: u32 = 5;

/// Wait after the first failure past [`FREE_FAILURES`], which doubles with each further failure.
const FIRST_FAILURE_DELAY: Duration = Duration::seconds(1);

/// Longest wait between two attempts with a credential from an address.
const MAX_FAILURE_DELAY: Duration = Duration::minutes(15);

/// Time after the last failure at which the server forgets the failures of an address.
const FAILURE_MEMORY: Duration = Duration::hours(1);

/// Number of client registrations that an address can make in each [`REGISTRATION_WINDOW`].
const REGISTRATIONS_PER_WINDOW: u32 = 10;

const REGISTRATION_WINDOW: Duration = Duration::hours(1);

/// Number of requests to the OAuth routes that read the database that an address can make in each [`REQUEST_WINDOW`].
const REQUESTS_PER_WINDOW: u32 = 60;

const REQUEST_WINDOW: Duration = Duration::minutes(1);

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
    since: DateTime<Utc>,
}

/// Rule of a limit on the events of each client address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// Failed passwords and bearer tokens delay the next attempt.
    Failures,
    /// Client registrations are limited per window.
    Registrations,
    /// Requests to the OAuth routes that read the database are limited per window, in memory only.
    Requests,
}

impl Rule {
    /// Return the name of this rule in the stored key of a count.
    fn name(self) -> &'static str {
        match self {
            Rule::Failures => "failures",
            Rule::Registrations => "registrations",
            Rule::Requests => "requests",
        }
    }

    /// Return the time after the start or last change of a count at which the count ends.
    fn memory(self) -> Duration {
        match self {
            Rule::Failures => FAILURE_MEMORY,
            Rule::Registrations => REGISTRATION_WINDOW,
            Rule::Requests => REQUEST_WINDOW,
        }
    }

    /// Return the count after an event at `now` that follows count `last`, or the time that the event must wait.
    fn next(self, last: Option<Count>, now: DateTime<Utc>) -> std::result::Result<Count, Duration> {
        match self {
            Rule::Failures => {
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
            Rule::Registrations | Rule::Requests => {
                let per_window = match self {
                    Rule::Requests => REQUESTS_PER_WINDOW,
                    _ => REGISTRATIONS_PER_WINDOW,
                };
                let window = last.unwrap_or(Count { n: 0, since: now });
                if window.n >= per_window {
                    return Err(self.memory() - (now - window.since));
                }
                Ok(Count {
                    n: window.n + 1,
                    ..window
                })
            }
        }
    }

    /// Return `count` if it has not ended at `now`.
    fn current(self, count: Option<Count>, now: DateTime<Utc>) -> Option<Count> {
        count.filter(|c| now - c.since < self.memory())
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

/// Limit of one rule on each client address, counted in memory and in the database.
///
/// The memory count stops a burst of requests before it reaches the database. The stored count keeps the limit across restarts and machines.
struct AddrLimit {
    rule: Rule,
    db: Db,
    local: Mutex<LocalCounts>,
}

/// Memory state of an [`AddrLimit`], for at most [`MAX_TRACKED_ADDRS`] addresses each.
#[derive(Default)]
struct LocalCounts {
    counts: HashMap<IpAddr, Count>,
    /// End of the last wait that the stored count of each address returned.
    stored_waits: HashMap<IpAddr, DateTime<Utc>>,
}

impl AddrLimit {
    fn new(rule: Rule, db: Db) -> Self {
        Self {
            rule,
            db,
            local: Mutex::default(),
        }
    }

    /// Count an event from `addr` at `now`, or return the time that the event must wait.
    async fn try_count(&self, addr: IpAddr, now: DateTime<Utc>) -> Result<Option<Duration>> {
        let key = addr_key(addr);
        if let Err(wait) = self.count_locally(key, now) {
            return Ok(Some(wait));
        }
        let stored_key = self.stored_key(key);
        let wait = run_txn!(&self.db, |txn| self
            .count_stored(&mut txn, &stored_key, now)
            .await)?;
        if let Some(wait) = wait {
            let mut local = self.lock();
            if local.stored_waits.len() >= MAX_TRACKED_ADDRS {
                local.stored_waits.retain(|_, end| *end > now);
            }
            if local.stored_waits.len() < MAX_TRACKED_ADDRS {
                local.stored_waits.insert(key, now + wait);
            }
        }
        Ok(wait)
    }

    /// Return the time that an event from `addr` at `now` must wait, as far as the memory of this server knows.
    fn local_wait(&self, addr: IpAddr, now: DateTime<Utc>) -> Option<Duration> {
        let key = addr_key(addr);
        let local = self.lock();
        let stored = local
            .stored_waits
            .get(&key)
            .filter(|end| **end > now)
            .map(|end| *end - now);
        let counted = self
            .rule
            .next(self.rule.current(local.counts.get(&key).copied(), now), now)
            .err();
        stored.max(counted)
    }

    /// Forget the count of `addr`.
    async fn reset(&self, addr: IpAddr) -> Result<()> {
        let key = addr_key(addr);
        {
            let mut local = self.lock();
            local.counts.remove(&key);
            local.stored_waits.remove(&key);
        }
        self.db
            .delete_oauth::<OAuthAddrCount>(&self.stored_key(key))
            .await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LocalCounts> {
        self.local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the key of the stored count of address key `key`.
    fn stored_key(&self, key: IpAddr) -> String {
        format!("{}/{key}", self.rule.name())
    }

    /// Count an event from address key `key` in memory, or return the time that the event must wait.
    fn count_locally(&self, key: IpAddr, now: DateTime<Utc>) -> std::result::Result<(), Duration> {
        let mut local = self.lock();
        if let Some(end) = local.stored_waits.get(&key).filter(|end| **end > now) {
            return Err(*end - now);
        }
        let counts = &mut local.counts;
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

    /// Count an event in the stored count `stored_key`, or return the time that the event must wait.
    async fn count_stored(
        &self,
        txn: &mut Txn,
        stored_key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Duration>> {
        let last = txn
            .get_oauth::<OAuthAddrCount>(stored_key)
            .await?
            .map(|row| Count {
                n: row.n,
                since: row.since,
            });
        let next = match self.rule.next(self.rule.current(last, now), now) {
            Ok(next) => next,
            Err(wait) => return Ok(Some(wait)),
        };
        let ttl = (next.since + self.rule.memory() - now)
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

/// Limits on failed credentials, client registrations, and OAuth requests from each client address.
#[derive(Clone)]
pub(crate) struct AddrLimits {
    client_addrs: ClientAddrSource,
    failures: Arc<AddrLimit>,
    registrations: Arc<AddrLimit>,
    requests: Arc<AddrLimit>,
}

impl AddrLimits {
    /// Create the limits of the clients that `client_addrs` names, with counts stored in `db`.
    pub(crate) fn new(db: &Db, client_addrs: ClientAddrSource) -> Self {
        Self {
            client_addrs,
            failures: Arc::new(AddrLimit::new(Rule::Failures, db.clone())),
            registrations: Arc::new(AddrLimit::new(Rule::Registrations, db.clone())),
            requests: Arc::new(AddrLimit::new(Rule::Requests, db.clone())),
        }
    }

    /// Return the address of the client that sent a request from `peer` with `headers`.
    pub(crate) fn client_addr(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        self.client_addrs.client_addr(peer, headers)
    }

    /// Count an attempt with a credential from `addr` as a failure until [`Self::record_success`], or return the time that `addr` must wait first.
    pub(crate) async fn try_credential(&self, addr: IpAddr) -> Result<Option<Duration>> {
        self.failures.try_count(addr, Utc::now()).await
    }

    /// Count a failed credential from `addr` only in memory, or return the time that `addr` must wait first.
    ///
    /// Use this for a credential that the server rejects without a database request.
    pub(crate) fn try_local_credential(&self, addr: IpAddr) -> Option<Duration> {
        self.failures
            .count_locally(addr_key(addr), Utc::now())
            .err()
    }

    /// Return the time that the next attempt with a credential from `addr` must wait, as far as the memory of this server knows.
    pub(crate) fn credential_wait(&self, addr: IpAddr) -> Option<Duration> {
        self.failures.local_wait(addr, Utc::now())
    }

    /// Forget the failures of `addr` after a correct credential.
    pub(crate) async fn record_success(&self, addr: IpAddr) -> Result<()> {
        self.failures.reset(addr).await
    }

    /// Count a request from `addr` to an OAuth route that reads the database, only in memory, or return the time until the window of `addr` ends if the window is full.
    pub(crate) fn try_request(&self, addr: IpAddr) -> Option<Duration> {
        self.requests
            .count_locally(addr_key(addr), Utc::now())
            .err()
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
    fn count_n(rule: Rule, mut last: Option<Count>, n: u32, now: DateTime<Utc>) -> Option<Count> {
        for _ in 0..n {
            last = Some(rule.next(rule.current(last, now), now).unwrap());
        }
        last
    }

    #[test]
    fn failure_waits_double_after_the_free_failures_up_to_a_limit() {
        let rule = Rule::Failures;
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
        let rule = Rule::Failures;
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
        let rule = Rule::Registrations;
        let t0 = Utc::now();
        let last = count_n(rule, None, REGISTRATIONS_PER_WINDOW, t0);
        let half = REGISTRATION_WINDOW / 2;
        assert_eq!(
            rule.next(rule.current(last, t0 + half), t0 + half),
            Err(half)
        );
        let next = t0 + REGISTRATION_WINDOW;
        assert!(rule.next(rule.current(last, next), next).is_ok());
    }

    #[test]
    fn requests_from_an_address_are_limited_per_minute() {
        let rule = Rule::Requests;
        let t0 = Utc::now();
        let last = count_n(rule, None, REQUESTS_PER_WINDOW, t0);
        assert_eq!(rule.next(last, t0), Err(REQUEST_WINDOW));
        let next = t0 + REQUEST_WINDOW;
        assert!(rule.next(rule.current(last, next), next).is_ok());
    }

    #[test]
    fn ipv6_addresses_of_one_network_share_a_key() {
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
        let cfg = crate::db::DbConfig {
            url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            db: "d".into(),
            cache_dir: None,
            curriculum: crate::graph::Curriculum::embedded(),
        };
        let limit = AddrLimit::new(Rule::Registrations, Db::connect(&cfg).unwrap());
        let t0 = Utc::now();
        for i in 0..=MAX_TRACKED_ADDRS {
            let addr = IpAddr::V4(u32::try_from(i).unwrap().into());
            limit.count_locally(addr, t0).unwrap();
        }
        assert_eq!(limit.lock().counts.len(), MAX_TRACKED_ADDRS);
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
        assert_eq!(retry_after_secs(Duration::milliseconds(1)), 1);
        assert_eq!(retry_after_secs(Duration::seconds(2)), 2);
    }
}
