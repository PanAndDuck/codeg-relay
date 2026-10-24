use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Unauthorized,
    Limited(Duration),
}
struct Failures {
    first: Instant,
    count: u32,
    blocked_until: Option<Instant>,
}
pub struct Limiter {
    peers: HashMap<IpAddr, Failures>,
    failures: u32,
    window: Duration,
    block: Duration,
}
impl Limiter {
    pub fn new(failures: u32, window: Duration, block: Duration) -> Self {
        Self {
            peers: HashMap::new(),
            failures,
            window,
            block,
        }
    }
    pub fn check(&mut self, ip: IpAddr, valid: bool, now: Instant) -> Decision {
        let ip = canonical_ip(ip);
        // Valid credentials never get locked out, including behind a shared NAT.
        // Preserve the IP's failures and block for other clients sharing it.
        if valid {
            return Decision::Allow;
        }
        if let Some(entry) = self.peers.get(&ip) {
            if let Some(until) = entry.blocked_until.filter(|until| *until > now) {
                return Decision::Limited(until.duration_since(now));
            }
            if entry.blocked_until.is_some() || now.duration_since(entry.first) >= self.window {
                self.peers.remove(&ip);
            }
        }
        // The relay supplies IPs; cap their bookkeeping even if a hostile relay
        // invents arbitrarily many addresses. Only failed credentials use slots.
        if self.peers.len() >= 4096 && !self.peers.contains_key(&ip) {
            if let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, entry)| entry.first)
                .map(|(ip, _)| *ip)
            {
                self.peers.remove(&oldest);
            }
        }
        let entry = self.peers.entry(ip).or_insert(Failures {
            first: now,
            count: 0,
            blocked_until: None,
        });
        entry.count = entry.count.saturating_add(1);
        if entry.count >= self.failures {
            entry.blocked_until = Some(now + self.block);
            Decision::Limited(self.block)
        } else {
            Decision::Unauthorized
        }
    }
}

pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_ip_churn_cannot_grow_failure_bookkeeping_without_bound() {
        let mut limiter = Limiter::new(10, Duration::from_secs(600), Duration::from_secs(900));
        let now = Instant::now();
        for n in 1..5000u32 {
            let _ = limiter.check(std::net::Ipv4Addr::from(n).into(), false, now);
        }
        assert_eq!(limiter.peers.len(), 4096);
        assert_eq!(
            limiter.check("203.0.113.50".parse().unwrap(), true, now),
            Decision::Allow
        );
    }
    #[test]
    fn bad_ip_blocks_then_expires_without_locking_out_valid_credentials() {
        let mut limiter = Limiter::new(3, Duration::from_secs(600), Duration::from_secs(900));
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        let now = Instant::now();
        assert_eq!(limiter.check(a, false, now), Decision::Unauthorized);
        assert_eq!(limiter.check(a, false, now), Decision::Unauthorized);
        assert_eq!(
            limiter.check(a, false, now),
            Decision::Limited(Duration::from_secs(900))
        );
        assert_eq!(limiter.check(b, true, now), Decision::Allow);
        assert_eq!(
            limiter.check(a, false, now + Duration::from_secs(900)),
            Decision::Unauthorized
        );
        assert_eq!(
            limiter.check(a, true, now + Duration::from_secs(901)),
            Decision::Allow
        );
    }
    #[test]
    fn valid_credentials_preserve_a_block_until_its_original_deadline() {
        let mut limiter = Limiter::new(10, Duration::from_secs(600), Duration::from_secs(900));
        let ip = "192.0.2.1".parse().unwrap();
        let now = Instant::now();
        for _ in 0..9 {
            assert_eq!(limiter.check(ip, false, now), Decision::Unauthorized);
        }
        assert_eq!(
            limiter.check(ip, false, now),
            Decision::Limited(Duration::from_secs(900))
        );
        assert_eq!(
            limiter.check(ip, true, now + Duration::from_secs(300)),
            Decision::Allow
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(300)),
            Decision::Limited(Duration::from_secs(600))
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(899)),
            Decision::Limited(Duration::from_secs(1))
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(900)),
            Decision::Unauthorized
        );
    }
    #[test]
    fn valid_credentials_preserve_failures_before_the_threshold() {
        let mut limiter = Limiter::new(2, Duration::from_secs(60), Duration::from_secs(90));
        let ip = "192.0.2.1".parse().unwrap();
        let now = Instant::now();
        assert_eq!(limiter.check(ip, false, now), Decision::Unauthorized);
        assert_eq!(
            limiter.check(ip, true, now + Duration::from_secs(30)),
            Decision::Allow
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(59)),
            Decision::Limited(Duration::from_secs(90))
        );
    }
    #[test]
    fn valid_credentials_do_not_restart_the_failure_window() {
        let mut limiter = Limiter::new(2, Duration::from_secs(60), Duration::from_secs(90));
        let ip = "192.0.2.1".parse().unwrap();
        let now = Instant::now();
        assert_eq!(limiter.check(ip, false, now), Decision::Unauthorized);
        assert_eq!(
            limiter.check(ip, true, now + Duration::from_secs(30)),
            Decision::Allow
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(60)),
            Decision::Unauthorized
        );
        assert_eq!(
            limiter.check(ip, false, now + Duration::from_secs(61)),
            Decision::Limited(Duration::from_secs(90))
        );
    }
    #[test]
    fn a_blocked_ips_valid_credentials_do_not_change_another_ips_failures() {
        let mut limiter = Limiter::new(2, Duration::from_secs(60), Duration::from_secs(90));
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        let now = Instant::now();
        assert_eq!(limiter.check(a, false, now), Decision::Unauthorized);
        assert_eq!(
            limiter.check(a, false, now),
            Decision::Limited(Duration::from_secs(90))
        );
        assert_eq!(limiter.check(b, false, now), Decision::Unauthorized);
        assert_eq!(limiter.check(a, true, now), Decision::Allow);
        assert_eq!(
            limiter.check(b, false, now),
            Decision::Limited(Duration::from_secs(90))
        );
        assert_eq!(limiter.check(b, true, now), Decision::Allow);
    }
    #[test]
    fn window_and_mapped_ipv4_cannot_bypass_failure_count() {
        let mut limiter = Limiter::new(2, Duration::from_secs(60), Duration::from_secs(90));
        let now = Instant::now();
        assert_eq!(
            limiter.check("192.0.2.1".parse().unwrap(), false, now),
            Decision::Unauthorized
        );
        assert!(matches!(
            limiter.check("::ffff:192.0.2.1".parse().unwrap(), false, now),
            Decision::Limited(_)
        ));
        assert_eq!(
            limiter.check("192.0.2.2".parse().unwrap(), false, now),
            Decision::Unauthorized
        );
        assert_eq!(
            limiter.check(
                "192.0.2.2".parse().unwrap(),
                false,
                now + Duration::from_secs(60)
            ),
            Decision::Unauthorized
        );
    }
}
