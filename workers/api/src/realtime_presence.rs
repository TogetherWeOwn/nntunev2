//! Inert account-scoped presence; no request routes, storage or playback controls.
//! Online status is a short-lived observation, not an authorization check.

use std::collections::BTreeMap;

use crate::session::{
    verify_stored_session, SessionClaims, SessionKeys, SessionRecord, MAX_SESSION_SECS,
};

const ONLINE_SECS: u64 = 45;
const MAX_DEVICES: usize = 32;

/// Inputs for one operation, not a reusable authentication proof. At each
/// serialized operation boundary the adapter supplies a freshly loaded server
/// row and nondecreasing server epoch seconds, never client-supplied values.
/// The verified login session identifies the device across ordinary refresh.
pub struct PresenceSession<'a> {
    pub keys: SessionKeys<'a>,
    pub token: &'a str,
    pub now: u64,
    pub current_row: Option<&'a SessionRecord>,
}

impl PresenceSession<'_> {
    fn verify(&self) -> Result<SessionClaims, PresenceError> {
        let row = self.current_row.ok_or(PresenceError::InvalidSession)?;
        verify_stored_session(&self.keys, self.token, self.now, row)
            .map_err(|_| PresenceError::InvalidSession)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct OnlineDevice {
    /// Account-local ordering, not a session ID or authorization credential.
    pub order: u64,
    pub is_current: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PresenceError {
    InvalidSession,
    WrongOwner,
    StaleSequence,
    Capacity,
}

struct Entry {
    order: u64,
    sequence: u64,
    online_until: u64,
    created_at: u64,
}

/// Replay protection is instance-local. A future adapter must preserve the
/// sequence markers across replacement or invalidate those login sessions.
pub struct Presence {
    owner: String,
    last_order: u64,
    devices: BTreeMap<String, Entry>,
}

impl Presence {
    pub fn new(session: PresenceSession<'_>) -> Result<Self, PresenceError> {
        Ok(Self {
            owner: session.verify()?.user_id,
            last_order: 0,
            devices: BTreeMap::new(),
        })
    }

    /// Apply a strictly increasing, nonzero sequence per login session. A
    /// stale frame does not renew presence. The caller serializes per owner.
    pub fn heartbeat(
        &mut self,
        session: PresenceSession<'_>,
        sequence: u64,
    ) -> Result<(), PresenceError> {
        let claims = self.verify_owner(&session)?;
        if sequence == 0 {
            return Err(PresenceError::StaleSequence);
        }
        let online_until = session
            .now
            .saturating_add(ONLINE_SECS)
            .min(claims.expires_at);
        if let Some(entry) = self.devices.get_mut(&claims.session_id) {
            if sequence <= entry.sequence {
                return Err(PresenceError::StaleSequence);
            }
            entry.sequence = sequence;
            entry.online_until = online_until;
            return Ok(());
        }

        // Expired presence is hidden, but sequence high-water marks survive
        // until even a refreshed credential for that session cannot be valid.
        self.devices
            .retain(|_, entry| session.now < entry.created_at.saturating_add(MAX_SESSION_SECS));
        if self.devices.len() >= MAX_DEVICES {
            return Err(PresenceError::Capacity);
        }
        let order = self
            .last_order
            .checked_add(1)
            .ok_or(PresenceError::Capacity)?;
        self.devices.insert(
            claims.session_id,
            Entry {
                order,
                sequence,
                online_until,
                created_at: claims.created_at,
            },
        );
        self.last_order = order;
        Ok(())
    }

    /// Resolve candidate peers from a fresh, owner-scoped server snapshot.
    /// Missing or invalid rows are offline without changing sequence markers.
    pub fn online<'a>(
        &self,
        session: PresenceSession<'_>,
        mut peer_row: impl FnMut(&str) -> Option<&'a SessionRecord>,
    ) -> Result<Vec<OnlineDevice>, PresenceError> {
        let claims = self.verify_owner(&session)?;
        let mut online: Vec<_> = self
            .devices
            .iter()
            .filter(|(_, entry)| session.now < entry.online_until)
            .filter_map(|(id, entry)| {
                let is_current = id == &claims.session_id;
                let row = if is_current {
                    session.current_row
                } else {
                    peer_row(id)
                }?;
                (row.id == *id
                    && row.user_id == self.owner
                    && row.created_at == entry.created_at
                    && !row.revoked
                    && session.now < row.expires_at)
                    .then_some(OnlineDevice {
                        order: entry.order,
                        is_current,
                    })
            })
            .collect();
        online.sort_unstable_by_key(|device| device.order);
        Ok(online)
    }

    fn verify_owner(&self, session: &PresenceSession<'_>) -> Result<SessionClaims, PresenceError> {
        let claims = session.verify()?;
        if claims.user_id != self.owner {
            return Err(PresenceError::WrongOwner);
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{issue_session, rotate_session, verify_session};

    const TEST_KEY: [u8; 32] = [0x5a; 32];
    const START: u64 = 100;

    struct TestSession {
        token: String,
        row: SessionRecord,
    }

    impl TestSession {
        fn new(owner: &str, issued: u64, ttl: u64) -> Self {
            let token = issue_session(&TEST_KEY, owner, issued, ttl).unwrap();
            let claims = verify_session(&TEST_KEY, &token, issued).unwrap();
            Self {
                token,
                row: SessionRecord {
                    id: claims.session_id,
                    user_id: claims.user_id,
                    created_at: claims.created_at,
                    expires_at: claims.expires_at,
                    rotation_counter: claims.rotation_counter,
                    revoked: false,
                },
            }
        }

        fn refresh(&mut self, now: u64, ttl: u64) {
            let keys = SessionKeys::single(&TEST_KEY);
            self.token = rotate_session(&keys, &self.token, now, ttl).unwrap();
            self.row.rotation_counter += 1;
            self.row.expires_at = now + ttl;
        }

        fn auth(&self, now: u64) -> PresenceSession<'_> {
            PresenceSession {
                keys: SessionKeys::single(&TEST_KEY),
                token: &self.token,
                now,
                current_row: Some(&self.row),
            }
        }
    }

    #[test]
    fn requires_a_current_live_row_for_each_operation() {
        let session = TestSession::new("alice", START, 10);
        let other = TestSession::new("bob", START, 10);
        let mut presence = Presence::new(session.auth(START)).unwrap();
        presence.heartbeat(session.auth(START), 1).unwrap();
        let mut revoked = session.row.clone();
        revoked.revoked = true;
        for (now, current_row) in [
            (START, None),
            (START, Some(&other.row)),
            (START, Some(&revoked)),
            (START + 10, Some(&session.row)),
        ] {
            let auth = || PresenceSession {
                current_row,
                ..session.auth(now)
            };
            assert!(matches!(
                Presence::new(auth()),
                Err(PresenceError::InvalidSession)
            ));
            assert_eq!(
                presence.heartbeat(auth(), 2),
                Err(PresenceError::InvalidSession)
            );
            assert_eq!(
                presence.online(auth(), |_| panic!("invalid caller")),
                Err(PresenceError::InvalidSession)
            );
        }
        assert_eq!(presence.devices[&session.row.id].sequence, 1);
    }

    #[test]
    fn isolates_accounts_and_orders_own_devices_by_first_heartbeat() {
        let first = TestSession::new("alice", START, 100);
        let second = TestSession::new("alice", START, 100);
        let foreign = TestSession::new("bob", START, 100);
        let mut presence = Presence::new(first.auth(START)).unwrap();

        assert_eq!(
            presence.heartbeat(foreign.auth(START), 1),
            Err(PresenceError::WrongOwner)
        );
        presence.heartbeat(second.auth(START), 9).unwrap();
        assert_eq!(
            presence.online(foreign.auth(START), |_| panic!("foreign caller")),
            Err(PresenceError::WrongOwner)
        );
        presence.heartbeat(first.auth(START), 1).unwrap();
        presence.heartbeat(second.auth(START + 1), 10).unwrap();
        assert_eq!(
            presence
                .online(first.auth(START + 1), |_| Some(&second.row))
                .unwrap(),
            [
                OnlineDevice {
                    order: 1,
                    is_current: false
                },
                OnlineDevice {
                    order: 2,
                    is_current: true
                },
            ]
        );
    }

    #[test]
    fn replay_cannot_extend_presence_or_reset_sequence_after_refresh() {
        let mut device = TestSession::new("alice", START, 10);
        let stale_token = device.token.clone();
        let mut presence = Presence::new(device.auth(START)).unwrap();
        assert_eq!(
            presence.heartbeat(device.auth(START), 0),
            Err(PresenceError::StaleSequence)
        );
        presence.heartbeat(device.auth(START), 7).unwrap();

        let now = START + 9;
        device.refresh(now, 100);
        let stale_auth = || PresenceSession {
            token: &stale_token,
            ..device.auth(now)
        };
        assert_eq!(
            presence.online(stale_auth(), |_| None),
            Err(PresenceError::InvalidSession)
        );
        assert_eq!(
            presence.heartbeat(stale_auth(), 8),
            Err(PresenceError::InvalidSession)
        );
        assert_eq!(
            presence.heartbeat(device.auth(now), 7),
            Err(PresenceError::StaleSequence)
        );

        let now = now + 1; // Original credential expired; new registration runs pruning.
        let peer = TestSession::new("alice", now, 100);
        presence.heartbeat(peer.auth(now), 1).unwrap();
        for stale in [7, 6] {
            assert_eq!(
                presence.heartbeat(device.auth(now), stale),
                Err(PresenceError::StaleSequence)
            );
        }
        assert_eq!(
            presence
                .online(peer.auth(now), |_| Some(&device.row))
                .unwrap(),
            [OnlineDevice {
                order: 2,
                is_current: true
            }]
        );
        presence.heartbeat(device.auth(now), 8).unwrap();
        assert_eq!(
            presence.online(device.auth(now), |_| None).unwrap(),
            [OnlineDevice {
                order: 1,
                is_current: true
            }]
        );
    }

    #[test]
    fn peer_snapshot_requires_a_live_current_row() {
        let mut first = TestSession::new("alice", START, 100);
        let second = TestSession::new("alice", START, 100);
        let mut presence = Presence::new(first.auth(START)).unwrap();
        presence.heartbeat(first.auth(START), 1).unwrap();
        first.refresh(START + 1, 9);
        let mut revoked = first.row.clone();
        revoked.revoked = true;
        let mut foreign = first.row.clone();
        foreign.user_id = "bob".into();
        let snapshot = |now, row| presence.online(second.auth(now), |_| row).unwrap();
        assert_eq!(snapshot(START + 9, Some(&first.row)).len(), 1);
        assert!(snapshot(START + 9, Some(&revoked)).is_empty());
        assert!(snapshot(START + 9, Some(&foreign)).is_empty());
        assert!(snapshot(START + 9, None).is_empty());
        assert!(snapshot(START + 10, Some(&first.row)).is_empty());
    }

    #[test]
    fn bounds_device_state_without_evicting_replay_markers_early() {
        let first = TestSession::new("alice", START, 100);
        let mut presence = Presence::new(first.auth(START)).unwrap();
        presence.heartbeat(first.auth(START), 1).unwrap();
        for _ in 1..MAX_DEVICES {
            let device = TestSession::new("alice", START, 100);
            presence.heartbeat(device.auth(START), 1).unwrap();
        }
        let now = START + ONLINE_SECS;
        let extra = TestSession::new("alice", now, 100);
        assert!(presence
            .online(extra.auth(now), |_| panic!("expired heartbeat"))
            .unwrap()
            .is_empty());
        assert_eq!(
            presence.heartbeat(extra.auth(now), 1),
            Err(PresenceError::Capacity)
        );
        assert_eq!(
            presence.heartbeat(first.auth(now), 1),
            Err(PresenceError::StaleSequence)
        );

        let now = START + MAX_SESSION_SECS;
        let fresh = TestSession::new("alice", now, 100);
        presence.heartbeat(fresh.auth(now), 1).unwrap();
        assert_eq!(presence.devices.len(), 1);
        assert_eq!(
            presence.online(fresh.auth(now), |_| None).unwrap(),
            [OnlineDevice {
                order: MAX_DEVICES as u64 + 1,
                is_current: true
            }]
        );
    }
}
