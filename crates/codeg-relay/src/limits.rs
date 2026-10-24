use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

pub struct Connections {
    inner: Mutex<Counts>,
    max: usize,
    per_ip: usize,
}
#[derive(Default)]
struct Counts {
    total: usize,
    ips: HashMap<IpAddr, usize>,
}
pub struct Permit {
    owner: Arc<Connections>,
    ip: IpAddr,
}
impl Connections {
    pub fn new(max: usize, per_ip: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::default(),
            max,
            per_ip,
        })
    }
    pub fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<Permit> {
        let mut counts = self.inner.lock().unwrap();
        if counts.total >= self.max || counts.ips.get(&ip).copied().unwrap_or(0) >= self.per_ip {
            return None;
        }
        counts.total += 1;
        *counts.ips.entry(ip).or_default() += 1;
        Some(Permit {
            owner: self.clone(),
            ip,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = self.owner.inner.lock().unwrap();
        counts.total -= 1;
        if let Some(n) = counts.ips.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                counts.ips.remove(&self.ip);
            }
        }
    }
}

#[derive(Default)]
pub struct AuthLimiter {
    entries: HashMap<IpAddr, Failures>,
}
#[derive(Default)]
struct Failures {
    times: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}
impl AuthLimiter {
    pub fn check(&mut self, ip: IpAddr, valid: bool) -> u16 {
        let now = Instant::now();
        self.entries.retain(|_, state| {
            state
                .times
                .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            state.blocked_until.is_some_and(|until| until > now) || !state.times.is_empty()
        });
        if self
            .entries
            .get(&ip)
            .is_some_and(|s| s.blocked_until.is_some_and(|t| t > now))
        {
            return 429;
        }
        if valid {
            return 200;
        }
        // Fail closed at capacity instead of evicting bans under an address flood.
        if !self.entries.contains_key(&ip) && self.entries.len() >= 4096 {
            return 429;
        }
        let entry = self.entries.entry(ip).or_default();
        entry.times.push_back(now);
        if entry.times.len() >= 10 {
            entry.blocked_until = Some(now + Duration::from_secs(300));
        }
        401
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permits_enforce_per_ip_and_global_limits_until_drop() {
        let connections = Connections::new(3, 2);
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        let first = connections.acquire(a).unwrap();
        let second = connections.acquire(a).unwrap();
        assert!(connections.acquire(a).is_none());
        let third = connections.acquire(b).unwrap();
        assert!(connections.acquire(b).is_none());
        drop(first);
        assert!(connections.acquire(a).is_some());
        drop((second, third));
        assert_eq!(connections.inner.lock().unwrap().total, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn auth_ban_expires_after_five_minutes_and_is_ip_scoped() {
        let ip = "127.0.0.1".parse().unwrap();
        let mut limiter = AuthLimiter::default();
        for _ in 0..10 {
            assert_eq!(limiter.check(ip, false), 401);
        }
        assert_eq!(limiter.check(ip, true), 429);
        assert_eq!(limiter.check("127.0.0.2".parse().unwrap(), true), 200);
        tokio::time::advance(Duration::from_secs(299)).await;
        assert_eq!(limiter.check(ip, false), 429);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(limiter.check(ip, true), 200);
        assert_eq!(limiter.check(ip, false), 401);
    }
    #[tokio::test(start_paused = true)]
    async fn old_auth_failures_do_not_count_in_new_minute() {
        let ip = "127.0.0.1".parse().unwrap();
        let mut limiter = AuthLimiter::default();
        for _ in 0..9 {
            assert_eq!(limiter.check(ip, false), 401);
        }
        tokio::time::advance(Duration::from_secs(60)).await;
        for _ in 0..9 {
            assert_eq!(limiter.check(ip, false), 401);
        }
        assert_eq!(limiter.check(ip, true), 200);
    }
}
