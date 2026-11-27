//! Partitioned engine-wide HTTP connection, DNS, and TLS session pool across
//! concurrent and sequential jobs (FP-151).
//!
//! # Architecture & thread-safety invariant
//!
//! Rather than sharing a single libcurl `CURLSH` across concurrent `Multi`
//! handles (where libcurl's internal `cpool->locked`, `Curl_cpool_add` pruning,
//! and Windows Schannel `multi->admin` credential refcounting are not safe
//! across concurrent `CURLM`s), each `(principal, CredentialScope)`
//! [`ConnectionPartition`] maintains a bounded checkout pool of idle
//! [`curl::multi::Multi`] handles.
//!
//! Each `Multi` owns its own connection cache (TCP keep-alive + HTTP/2), DNS
//! cache, and TLS session cache exclusively on one thread while checked out.
//! When a transfer finishes untainted, every `Easy2` handle is removed and
//! dropped before the idle `Multi` (with zero attached transfers) is returned
//! to its partition's checkout pool. Per `libcurl-thread(3)`, moving exclusive
//! ownership of an idle handle between threads is sound because no two threads
//! ever access the same `CURLM` simultaneously.
//!
//! # Isolation invariant
//!
//! Partitions are keyed by `(principal, CredentialScope)`. Cookies are never
//! stored on `Multi` handles (`CURLOPT_COOKIEFILE` lives only on per-request
//! `Easy2` handles). Credentialed transfers (any cookies, referer, custom
//! headers, URL userinfo, or proxy overrides) are refused by
//! [`is_pool_eligible`] and never check out from a shared partition; any
//! redirect that acquires a `Set-Cookie` or userinfo taints the checkout so
//! its `Multi` is dropped rather than returned to the pool.

use crate::{RequestContext, TransferError};
use curl::multi::Multi;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use url::Url;

/// Maximum number of distinct `(principal, Public)` partitions cached per
/// [`ConnectionPool`]. Idle least-recently-used partitions are evicted above
/// this cap so memory stays bounded even with ephemeral agent names.
const MAX_PARTITIONS: usize = 64;

/// Maximum number of idle `Multi` handles retained per partition (matching
/// `MAX_ACTIVE_DOWNLOADS`). Each idle `Multi` holds warm TCP keep-alive
/// sockets, DNS entries, and TLS sessions for its `(principal, Public)` scope.
const MAX_IDLE_MULTIS: usize = 8;

static NEXT_PARTITION_ID: AtomicU64 = AtomicU64::new(1);

/// Credential isolation scope for a connection pool partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CredentialScope {
    /// Public, credential-free origin transfer. Eligible to reuse DNS, TCP,
    /// and TLS sessions with other public transfers of the *same* principal.
    Public,
    /// Dedicated non-shared scope identifier (never shares with `Public` or
    /// any other scope).
    Isolated(u64),
}

/// Partition key isolating DNS, TCP connections, and TLS sessions by
/// `(principal, credential_scope)`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PoolKey {
    pub principal: String,
    pub scope: CredentialScope,
}

impl PoolKey {
    pub fn public(principal: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            scope: CredentialScope::Public,
        }
    }

    pub fn isolated(principal: impl Into<String>, scope_id: u64) -> Self {
        Self {
            principal: principal.into(),
            scope: CredentialScope::Isolated(scope_id),
        }
    }
}

/// Returns `true` only when a request carries zero credentials, cookies,
/// referer, custom headers, URL userinfo, or proxy overrides and is therefore
/// safe to use a shared `(principal, Public)` pool partition.
pub fn is_pool_eligible(context: &RequestContext, url: &Url) -> bool {
    context.cookie_lines.is_empty()
        && context.referer.is_none()
        && context.custom_headers.is_empty()
        && context.proxy.is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

/// Exclusive ownership wrapper for an idle [`curl::multi::Multi`] with zero
/// attached easy handles.
///
/// # Safety
/// `PooledMulti` is `Send` (never `Sync`): it only ever moves exclusive
/// ownership of a zero-transfer `CURLM` between threads through a `Mutex`.
/// Per `libcurl-thread(3)`, transferring an idle handle between threads is
/// safe so long as only one thread accesses it at a time.
struct PooledMulti(Multi);

// SAFETY: Exclusive ownership of an idle `Multi` with zero attached easy
// handles may move between threads (guarded by `Mutex`, never shared
// concurrently; `PooledMulti` is intentionally `!Sync`).
unsafe impl Send for PooledMulti {}

/// One isolated `(principal, credential_scope)` connection, DNS, and TLS
/// session pool backed by checked-out idle `curl::multi::Multi` handles.
pub struct ConnectionPartition {
    id: u64,
    key: PoolKey,
    idle: Mutex<Vec<PooledMulti>>,
}

impl std::fmt::Debug for ConnectionPartition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let idle = self.idle.lock().map_or(0, |v| v.len());
        f.debug_struct("ConnectionPartition")
            .field("id", &self.id)
            .field("scope", &self.key.scope)
            .field("idle_handles", &idle)
            .finish()
    }
}

impl ConnectionPartition {
    /// Creates a standalone partition for `key`.
    pub fn new(key: PoolKey) -> Result<Self, TransferError> {
        curl::init();
        Ok(Self {
            id: NEXT_PARTITION_ID.fetch_add(1, Ordering::Relaxed),
            key,
            idle: Mutex::new(Vec::new()),
        })
    }

    /// Opaque monotonic identifier for diagnostics and isolation tests.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The `(principal, credential_scope)` key that owns this partition.
    pub fn key(&self) -> &PoolKey {
        &self.key
    }

    /// Checks out a warm idle `Multi` handle from this partition, or creates a
    /// fresh one when none are idle.
    pub(crate) fn checkout(&self) -> Result<Multi, TransferError> {
        if let Some(PooledMulti(multi)) = self.idle.lock().unwrap_or_else(|e| e.into_inner()).pop()
        {
            return Ok(multi);
        }
        crate::scheduler::new_multi()
    }

    /// Returns a clean, untainted `Multi` handle (with ZERO attached easy
    /// handles) to this partition for reuse by future jobs in the same scope.
    pub(crate) fn checkin(&self, mut multi: Multi) {
        // Restore default HTTP/2 multiplexing in case an H2 independent-socket
        // trial turned it off during the previous transfer.
        if multi.pipelining(false, true).is_err() {
            return;
        }
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.len() < MAX_IDLE_MULTIS {
            idle.push(PooledMulti(multi));
        }
        // Excess handles drop cleanly at scope exit.
    }
}

struct PoolEntry {
    partition: Arc<ConnectionPartition>,
    last_used: u64,
}

/// Engine-wide registry of `(principal, credential_scope)` connection pool
/// partitions (FP-151).
#[derive(Default)]
pub struct ConnectionPool {
    entries: Mutex<HashMap<PoolKey, PoolEntry>>,
    clock: AtomicU64,
}

impl std::fmt::Debug for ConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.entries.lock().map_or(0, |map| map.len());
        f.debug_struct("ConnectionPool")
            .field("partitions", &count)
            .finish()
    }
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the shared [`ConnectionPartition`] for `key`, creating it on
    /// first use. `CredentialScope::Isolated(_)` keys always return a fresh
    /// uncached partition so credentialed transfers can never share state.
    pub fn partition(&self, key: PoolKey) -> Result<Arc<ConnectionPartition>, TransferError> {
        if matches!(key.scope, CredentialScope::Isolated(_)) {
            return ConnectionPartition::new(key).map(Arc::new);
        }
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(&key) {
            entry.last_used = tick;
            return Ok(Arc::clone(&entry.partition));
        }
        // Evict the least-recently-used idle partition if at capacity.
        if entries.len() >= MAX_PARTITIONS
            && let Some(victim) = entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.partition) == 1)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone())
        {
            entries.remove(&victim);
        }
        let partition = Arc::new(ConnectionPartition::new(key.clone())?);
        entries.insert(
            key,
            PoolEntry {
                partition: Arc::clone(&partition),
                last_used: tick,
            },
        );
        Ok(partition)
    }

    /// Number of cached public partitions currently held by this pool.
    pub fn partition_count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_isolates_distinct_principals_and_never_caches_isolated_scopes() {
        let pool = ConnectionPool::new();
        let user_a = pool.partition(PoolKey::public("user")).unwrap();
        let user_b = pool.partition(PoolKey::public("user")).unwrap();
        let agent_a = pool.partition(PoolKey::public("agent:alpha")).unwrap();
        let agent_b = pool.partition(PoolKey::public("agent:beta")).unwrap();
        let iso_1 = pool.partition(PoolKey::isolated("user", 1)).unwrap();
        let iso_2 = pool.partition(PoolKey::isolated("user", 1)).unwrap();

        assert_eq!(user_a.id(), user_b.id());
        assert_ne!(user_a.id(), agent_a.id());
        assert_ne!(agent_a.id(), agent_b.id());
        assert_ne!(iso_1.id(), iso_2.id());
        assert_ne!(iso_1.id(), user_a.id());
        assert_eq!(pool.partition_count(), 3);
    }

    #[test]
    fn eligibility_rejects_cookies_referer_custom_headers_proxy_and_userinfo() {
        let public_url = Url::parse("https://files.example.test/a.bin").unwrap();
        let userinfo_url = Url::parse("https://alice:secret@files.example.test/a.bin").unwrap();

        assert!(is_pool_eligible(&RequestContext::default(), &public_url));
        assert!(!is_pool_eligible(&RequestContext::default(), &userinfo_url));

        let with_ua = RequestContext {
            user_agent: Some("Fetchpath/2.0".into()),
            ..RequestContext::default()
        };
        assert!(is_pool_eligible(&with_ua, &public_url));

        let with_cookie = RequestContext {
            cookie_lines: vec!["files.example.test\tFALSE\t/\tFALSE\t0\tsid\tval".into()],
            ..RequestContext::default()
        };
        assert!(!is_pool_eligible(&with_cookie, &public_url));

        let with_referer = RequestContext {
            referer: Some("https://files.example.test/page".into()),
            ..RequestContext::default()
        };
        assert!(!is_pool_eligible(&with_referer, &public_url));

        let with_header = RequestContext {
            custom_headers: vec![("Authorization".into(), "Bearer tok".into())],
            ..RequestContext::default()
        };
        assert!(!is_pool_eligible(&with_header, &public_url));

        let with_proxy = RequestContext {
            proxy: Some("http://127.0.0.1:8080".into()),
            ..RequestContext::default()
        };
        assert!(!is_pool_eligible(&with_proxy, &public_url));
    }
}
