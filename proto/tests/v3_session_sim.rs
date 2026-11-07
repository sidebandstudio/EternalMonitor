//! v3 reconnect state machines, host and iPad together, on a simulated clock
//! and network (`v3_sim`). Each test pins a row of the session tables or the
//! timing rule; nothing depends on real time or machine load.

mod v3_sim;

use std::time::Duration;

use eternal_wire::v2::PacketType;
use eternal_wire::v3::client::{ClientClose, ClientConfig, ClientKey};
use eternal_wire::v3::host::{CloseReason, HostConfig};
use v3_sim::*;

fn live_ids(sim: &Sim) -> Vec<u32> {
    sim.events
        .iter()
        .filter_map(|e| match e {
            Event::IpadLive { session_id, .. } => Some(*session_id),
            _ => None,
        })
        .collect()
}

fn first_at(sim: &Sim, pred: impl Fn(&Event) -> bool) -> Option<Duration> {
    sim.events.iter().find(|e| pred(e)).map(Event::at)
}

fn connect_live(sim: &mut Sim) -> u32 {
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s.both_live_on_same_session()));
    sim.run_for(ms(100));
    sim.host_live().unwrap()
}

#[test]
fn timing_rule_every_retry_goes_out_while_its_pending_exists() {
    let host = HostConfig::new(HOST_ID);
    let client = ClientConfig::new(HOST_ID);
    assert!(client.msg1_window + client.confirm_window < host.pending_lifetime);
    assert_eq!(host.pending_lifetime, ms(5000));
    assert_eq!(client.msg1_window, ms(2000));
    assert_eq!(client.confirm_window, ms(2000));
    assert_eq!(client.msg1_retry_interval, ms(250));
    assert_eq!(client.confirm_retry_interval, ms(100));
    assert_eq!(client.drain_limit, ms(1000));
}

#[test]
fn clean_connect_commits_once_and_acks_with_the_status() {
    let mut sim = Sim::new(1);
    let start = sim.now;
    sim.connect();
    assert!(sim.run_until(ms(500), |s| s.both_live_on_same_session()));
    sim.run_for(ms(500));
    assert_eq!(sim.commits().len(), 1);
    let live_at = first_at(&sim, |e| matches!(e, Event::IpadLive { .. })).unwrap();
    assert!(live_at - start <= ms(60), "{:?}", live_at - start);
    assert!(first_at(&sim, |e| matches!(e, Event::IpadStatus { .. })).is_some());
    let released = first_at(&sim, |e| matches!(e, Event::IpadReleased { .. })).unwrap();
    assert!(
        released <= live_at,
        "cutover releases input before going live"
    );
    assert!(!sim.client.is_confirming());
    // Reports reach the host under the live session.
    assert!(sim.events.iter().any(|e| matches!(e,
        Event::HostDelivered { inner_type, .. } if *inner_type == PacketType::ReceiverReport as u8)));
}

/// The worst case the timing rule allows: only the last message 1 copy that
/// can still be answered inside the 2 s window gets through, and only the
/// last `SessionConfirm` of its 2 s window. The pending must still exist, so
/// that confirm commits. When the ack then can't make it back inside the
/// iPad's window, its next attempt replaces the session, as the table says.
#[test]
fn the_last_message_1_and_the_last_confirm_still_commit() {
    for latency_ms in [5u64, 20, 60, 120, 400, 900] {
        let latency = ms(latency_ms);
        let rtt_ticks = 2 * latency + 2 * TICK;
        // Last copy j (sent at j × 250 ms) whose message 2 is back before 2 s.
        let last_msg1 = (0..8u32)
            .rev()
            .find(|j| ms(250) * *j + rtt_ticks < ms(2000))
            .unwrap() as usize;
        let mut sim = Sim::new(10 + latency_ms);
        sim.latency = latency;
        sim.policy.media_every = None;
        // Faults hit the first attempt only.
        sim.fault = Box::new(move |sent| match sent.kind {
            Kind::Msg1 if sent.nth < 8 && sent.nth != last_msg1 => Fate::Drop,
            Kind::Confirm if sent.nth < 20 && sent.nth != 19 => Fate::Drop,
            _ => Fate::Deliver,
        });
        sim.connect();
        assert!(
            sim.run_until(ms(6000), |s| !s.commits().is_empty()),
            "latency {latency_ms} ms: the last confirm found no pending"
        );
        let confirms = sim.sent_of(Dir::ToHost, Kind::Confirm);
        assert_eq!(confirms.len(), 20, "latency {latency_ms} ms");
        let commit = first_at(&sim, |e| matches!(e, Event::HostCommitted { .. })).unwrap();
        assert_eq!(commit, confirms[19].at + latency, "latency {latency_ms} ms");
        assert!(
            sim.run_until(ms(10_000), |s| s.both_live_on_same_session()),
            "latency {latency_ms} ms"
        );
    }
}

#[test]
fn a_lost_message_2_is_resent_byte_for_byte_from_the_cache() {
    let mut sim = Sim::new(2);
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::Msg2 if sent.nth < 2 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    assert!(sim.run_until(ms(2000), |s| s.both_live_on_same_session()));
    let msg2s = sim.sent_of(Dir::ToIpad, Kind::Msg2);
    assert_eq!(msg2s.len(), 3);
    assert_eq!(msg2s[0].datagram, msg2s[1].datagram);
    assert_eq!(msg2s[1].datagram, msg2s[2].datagram);
    let msg1s = sim.sent_of(Dir::ToHost, Kind::Msg1);
    assert_eq!(msg1s.len(), 3);
    assert!(msg1s.windows(2).all(|w| w[0].datagram == w[1].datagram));
    assert_eq!(sim.commits().len(), 1);
}

#[test]
fn all_confirms_lost_starts_a_new_attempt_and_the_first_pending_never_commits() {
    let mut sim = Sim::new(3);
    sim.policy.media_every = None;
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::Confirm if sent.nth < 20 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    sim.run_for(ms(100));
    let first = sim.host.pending_session(&sim.key.dev_id).unwrap();
    assert!(sim.run_until(ms(6000), |s| s.both_live_on_same_session()));
    assert_eq!(sim.commits().len(), 1);
    assert_ne!(sim.host_live(), Some(first));
    assert_eq!(
        sim.sent_of(Dir::ToHost, Kind::Msg1).len(),
        2,
        "one new attempt"
    );
    let msg1s = sim.sent_of(Dir::ToHost, Kind::Msg1);
    assert_ne!(
        msg1s[0].datagram, msg1s[1].datagram,
        "new ephemeral and timestamp"
    );
}

#[test]
fn reconnect_keeps_the_old_session_until_commit_and_zeroes_it_after() {
    let mut sim = Sim::new(4);
    sim.policy.media_every = None;
    let old = connect_live(&mut sim);
    // Hold back a report sealed under the old session.
    let held = sim
        .client
        .seal(sim.now, PacketType::ReceiverReport as u8, &[0; REPORT_BODY])
        .unwrap();
    // Drop every confirm for a while so the cutover is observable.
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::Confirm if sent.nth < 5 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    sim.run_until(ms(200), |s| s.client.is_confirming());
    assert!(sim.client.is_confirming());
    assert_eq!(
        sim.host_live(),
        Some(old),
        "old session untouched before commit"
    );
    assert_eq!(sim.client.live_session(), Some(old));
    assert!(
        sim.client
            .seal(sim.now, PacketType::InputEvent as u8, &[0; 30])
            .is_none(),
        "no input across the cutover"
    );
    assert!(sim.run_until(ms(2000), |s| s.host_live() != Some(old)
        && s.both_live_on_same_session()));
    let new = sim.host_live().unwrap();
    assert!(sim.events.iter().any(|e| *e
        == Event::HostClosed {
            at: first_at(
                &sim,
                |e| matches!(e, Event::HostCommitted { session_id, .. } if *session_id == new)
            )
            .unwrap(),
            session_id: old,
            reason: CloseReason::Superseded,
        }));
    // The old session's keys are gone on the host: its record is dropped.
    let delivered_before = sim
        .events
        .iter()
        .filter(|e| matches!(e, Event::HostDelivered { session_id, .. } if *session_id == old))
        .count();
    sim.inject(Dir::ToHost, &held);
    let delivered_after = sim
        .events
        .iter()
        .filter(|e| matches!(e, Event::HostDelivered { session_id, .. } if *session_id == old))
        .count();
    assert_eq!(delivered_before, delivered_after);
    assert_eq!(sim.commits().len(), 2);
}

#[test]
fn the_old_session_drains_until_the_new_idr_decodes() {
    let mut sim = Sim::new(5);
    sim.policy.media_every = None;
    connect_live(&mut sim);
    let dev_id = sim.key.dev_id;
    let held: Vec<Vec<u8>> = (0..3)
        .map(|_| {
            sim.host
                .seal(sim.now, &dev_id, PacketType::Media as u8, &[1; MEDIA_BODY])
                .unwrap()
                .1
        })
        .collect();
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s.client.draining_session().is_some()));
    let old = sim.client.draining_session().expect("old session drains");
    let new = sim.client.live_session().unwrap();

    sim.inject(Dir::ToIpad, &held[0]);
    assert!(sim.events.contains(&Event::IpadDelivered {
        at: sim.now,
        session_id: old,
        draining: true
    }));
    let idr = sim
        .host
        .seal(sim.now, &dev_id, PacketType::Media as u8, &[2; MEDIA_BODY])
        .unwrap()
        .1;
    sim.inject(Dir::ToIpad, &idr);
    assert!(sim.events.contains(&Event::IpadClosed {
        at: sim.now,
        session_id: old,
        reason: ClientClose::DrainEnded
    }));
    let count = sim.events.len();
    sim.inject(Dir::ToIpad, &held[1]);
    assert_eq!(sim.events.len(), count, "old media dropped after the IDR");
    assert_eq!(sim.client.live_session(), Some(new));
}

#[test]
fn the_drain_ends_after_1_s_without_an_idr() {
    let mut sim = Sim::new(6);
    sim.policy.media_every = None;
    connect_live(&mut sim);
    let dev_id = sim.key.dev_id;
    let held = sim
        .host
        .seal(sim.now, &dev_id, PacketType::Media as u8, &[1; MEDIA_BODY])
        .unwrap()
        .1;
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s.client.draining_session().is_some()));
    let old = sim.client.draining_session().unwrap();
    let drain_start = first_at(&sim, |e| matches!(e, Event::IpadLive { session_id, .. } if Some(*session_id) == sim.client.live_session())).unwrap();
    sim.run_until(ms(2000), |s| s.client.draining_session().is_none());
    let ended = first_at(&sim, |e| {
        matches!(e, Event::IpadClosed { session_id, reason: ClientClose::DrainEnded, .. } if *session_id == old)
    })
    .unwrap();
    assert_eq!(ended - drain_start, ms(1000));
    let count = sim.events.len();
    sim.inject(Dir::ToIpad, &held);
    assert_eq!(sim.events.len(), count);
}

#[test]
fn replayed_handshakes_and_records_never_commit_or_deliver() {
    let mut sim = Sim::new(7);
    let live = connect_live(&mut sim);
    let captured: Vec<Vec<u8>> = sim
        .sent
        .iter()
        .filter(|s| s.dir == Dir::ToHost)
        .map(|s| s.datagram.clone())
        .collect();
    let host_events = |sim: &Sim| {
        sim.events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::HostCommitted { .. }
                        | Event::HostDelivered { .. }
                        | Event::HostConfirmRequested { .. }
                )
            })
            .count()
    };
    let before = host_events(&sim);
    let sends_before = sim.sent.len();
    for datagram in &captured {
        sim.inject(Dir::ToHost, datagram);
    }
    assert_eq!(host_events(&sim), before, "replayed records are dropped");
    assert_eq!(sim.sent.len(), sends_before, "and get no reply");
    assert_eq!(sim.host_live(), Some(live));

    // After a reconnect, the first session's message 1 is stale: it gets an
    // encrypted Stale answer at most, never a pending session.
    let first_msg1 = sim.sent_of(Dir::ToHost, Kind::Msg1)[0].datagram.clone();
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s.host_live() != Some(live)
        && s.both_live_on_same_session()));
    sim.run_for(ms(300));
    sim.inject(Dir::ToHost, &first_msg1);
    assert!(sim.host.pending_session(&sim.key.dev_id).is_none());
    sim.run_for(ms(6000));
    assert_eq!(sim.commits().len(), 2);
}

#[test]
fn a_finalize_conflict_is_refused_with_identical_resends_and_the_live_session_survives() {
    let mut sim = Sim::new(8);
    let live = connect_live(&mut sim);
    // A second key whose record will conflict at Finalize.
    let other = ClientKey {
        dev_id: [0x77; 16],
        k_dev: [0x88; 32],
    };
    sim.store.keys.insert(other.dev_id, other.k_dev);
    sim.policy.refuse = true;
    // Lose the first refusal, so a confirm retry draws a resend.
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::AckOrRefused if sent.nth == 1 => Fate::Drop,
        _ => Fate::Deliver,
    });
    let out = sim.client.connect(sim.now, other.clone(), sim.clock_us());
    for o in out {
        if let eternal_wire::v3::client::ClientOutput::Send(d) = o {
            sim.inject(Dir::ToHost, &d);
        }
    }
    assert!(sim.run_until(ms(1000), |s| s
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadRefused { .. }))));
    let refused = sim
        .events
        .iter()
        .find_map(|e| match e {
            Event::IpadRefused { status, .. } => Some(status.clone()),
            _ => None,
        })
        .unwrap();
    assert!(refused.tombstones.contains(&other.dev_id));
    let refusals = sim.sent_of(Dir::ToIpad, Kind::AckOrRefused);
    assert_eq!(refusals.len(), 3, "first ack, refusal, one resend");
    assert_eq!(
        refusals[1].datagram, refusals[2].datagram,
        "identical bytes"
    );
    assert!(refusals[2].at - refusals[1].at >= ms(100));
    assert_eq!(sim.host_live(), Some(live));
    assert_eq!(sim.client.live_session(), Some(live));
    sim.run_for(ms(3000));
    assert!(sim.both_live_on_same_session());
    assert_eq!(sim.host_live(), Some(live));
}

#[test]
fn the_ack_waits_for_the_durable_writes_and_retries_are_rate_limited() {
    let mut config = ClientConfig::new(HOST_ID);
    config.confirm_retry_interval = ms(20);
    let mut sim = Sim::with_configs(9, HostConfig::new(HOST_ID), config);
    sim.policy.media_every = None;
    sim.policy.durable_delay = ms(300);
    // Lose every ack after the first durable one, so retries continue.
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::AckOrRefused => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    // Stay inside the 2 s confirm window: after it the iPad starts over.
    sim.run_for(ms(1900));
    let commit_at = first_at(&sim, |e| matches!(e, Event::HostCommitted { .. })).unwrap();
    let acks = sim.sent_of(Dir::ToIpad, Kind::AckOrRefused);
    assert!(
        acks.iter().all(|a| a.at >= commit_at + ms(300)),
        "no ack before durable"
    );
    assert_eq!(acks[0].at, commit_at + ms(300));
    assert!(
        acks.windows(2).all(|w| w[1].at - w[0].at >= ms(50)),
        "at most one per 50 ms"
    );
    let confirms_after = sim
        .sent_of(Dir::ToHost, Kind::Confirm)
        .iter()
        .filter(|c| c.at >= commit_at + ms(300))
        .count();
    assert!(
        acks.len() < confirms_after,
        "{} acks for {confirms_after} confirms",
        acks.len()
    );
    assert_eq!(sim.commits().len(), 1, "no second commit from retries");
}

#[test]
fn media_before_the_ack_makes_the_session_live_and_confirms_continue_to_the_window_end() {
    let mut sim = Sim::new(10);
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::AckOrRefused => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    assert!(sim.run_until(ms(500), |s| s.both_live_on_same_session()));
    sim.run_for(ms(3000));
    assert!(sim.both_live_on_same_session());
    let confirms = sim.sent_of(Dir::ToHost, Kind::Confirm);
    assert_eq!(
        confirms.len(),
        20,
        "100 ms retries for the whole 2 s window"
    );
    assert!(confirms.last().unwrap().at - confirms[0].at < ms(2000));
    assert_eq!(sim.commits().len(), 1);
}

#[test]
fn silence_from_the_ipad_ends_the_session_after_the_liveness_timeout() {
    let mut sim = Sim::new(11);
    sim.policy.report_every = None;
    let live = connect_live(&mut sim);
    sim.run_for(ms(4000));
    let last_rx = sim
        .sent_of(Dir::ToHost, Kind::Confirm)
        .last()
        .map(|c| c.at + sim.latency)
        .unwrap();
    let closed = first_at(&sim, |e| {
        matches!(e, Event::HostClosed { session_id, reason: CloseReason::Liveness, .. } if *session_id == live)
    })
    .unwrap();
    assert!(
        closed - last_rx >= ms(3000) && closed - last_rx <= ms(3000) + 2 * TICK,
        "{:?}",
        closed - last_rx
    );
    assert!(sim.events.iter().any(|e| matches!(e,
        Event::IpadClosed { session_id, reason: ClientClose::HostBye(_), .. } if *session_id == live)));
}

#[test]
fn the_ipad_reconnects_before_the_maximum_age() {
    let mut host = HostConfig::new(HOST_ID);
    host.max_age = Duration::from_secs(60);
    let mut client = ClientConfig::new(HOST_ID);
    client.max_age = Duration::from_secs(60);
    client.reconnect_margin = Duration::from_secs(5);
    let mut sim = Sim::with_configs(12, host, client);
    let first = connect_live(&mut sim);
    let first_commit = first_at(&sim, |e| matches!(e, Event::HostCommitted { .. })).unwrap();
    sim.run_for(Duration::from_secs(70));
    let commits = sim.commits();
    assert!(commits.len() >= 2);
    assert_ne!(commits[1], first);
    let second_commit = sim
        .events
        .iter()
        .filter_map(|e| match e {
            Event::HostCommitted { at, .. } => Some(*at),
            _ => None,
        })
        .nth(1)
        .unwrap();
    assert!(second_commit - first_commit < Duration::from_secs(60));
    assert!(second_commit - first_commit >= Duration::from_secs(55));
    assert!(!sim.events.iter().any(|e| matches!(
        e,
        Event::HostClosed {
            reason: CloseReason::Expired,
            ..
        }
    )));
    assert!(sim.both_live_on_same_session());
}

#[test]
fn past_the_maximum_age_the_host_ends_the_session() {
    let mut host = HostConfig::new(HOST_ID);
    host.max_age = Duration::from_secs(60);
    let mut client = ClientConfig::new(HOST_ID);
    client.max_age = Duration::from_secs(600);
    client.reconnect_margin = Duration::ZERO;
    let mut sim = Sim::with_configs(13, host, client);
    let live = connect_live(&mut sim);
    let commit = first_at(&sim, |e| matches!(e, Event::HostCommitted { .. })).unwrap();
    sim.run_for(Duration::from_secs(61));
    let closed = first_at(&sim, |e| {
        matches!(e, Event::HostClosed { session_id, reason: CloseReason::Expired, .. } if *session_id == live)
    })
    .unwrap();
    assert!(
        closed - commit >= Duration::from_secs(60)
            && closed - commit <= Duration::from_secs(60) + TICK
    );
    assert!(sim
        .host
        .seal(sim.now, &sim.key.dev_id, PacketType::Media as u8, &[0; 10])
        .is_none());
}

#[test]
fn a_rolled_back_clock_gets_stale_then_one_retry_commits() {
    let mut sim = Sim::new(14);
    let ahead = sim.clock_us() + 3_600_000_000;
    sim.store
        .timestamps
        .borrow_mut()
        .insert(sim.key.dev_id, ahead);
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s.both_live_on_same_session()));
    assert_eq!(sim.sent_of(Dir::ToHost, Kind::Msg1).len(), 2);
    assert_eq!(sim.sent_of(Dir::ToIpad, Kind::Msg2).len(), 2);
    assert!(sim.client.last_timestamp() > ahead);
    assert_eq!(sim.commits().len(), 1);
}

#[test]
fn soak_with_loss_duplication_and_reordering() {
    for seed in 0..40u64 {
        let mut sim = Sim::new(100 + seed);
        let mut rng = XorShift(seed * 7919 + 1);
        sim.latency = ms(5 + rng.next() % 60);
        let mut fault_rng = XorShift(seed * 104_729 + 3);
        sim.fault = Box::new(move |_| match fault_rng.next() % 100 {
            0..=4 => Fate::Drop,
            5..=6 => Fate::Duplicate {
                extra: ms(fault_rng.next() % 40),
            },
            7..=9 => Fate::Delay {
                extra: ms(fault_rng.next() % 80),
            },
            _ => Fate::Deliver,
        });
        sim.connect();
        for round in 0..4 {
            assert!(
                sim.run_until(Duration::from_secs(8), |s| s.both_live_on_same_session()),
                "seed {seed} round {round}"
            );
            sim.run_for(ms(1000 + rng.next() % 4000));
            sim.connect();
        }
        assert!(
            sim.run_until(Duration::from_secs(10), |s| s.both_live_on_same_session()
                && !s.client.is_connecting()
                && !s.client.is_confirming()),
            "seed {seed}"
        );
        sim.check_live_only_on_commits();
    }
}

#[test]
fn after_a_refusal_the_automatic_reconnect_uses_the_live_sessions_key() {
    let mut host = HostConfig::new(HOST_ID);
    host.max_age = Duration::from_secs(30);
    let mut client = ClientConfig::new(HOST_ID);
    client.max_age = Duration::from_secs(30);
    client.reconnect_margin = Duration::from_secs(5);
    let mut sim = Sim::with_configs(15, host, client);
    let first = connect_live(&mut sim);
    let other = ClientKey {
        dev_id: [0x66; 16],
        k_dev: [0x99; 32],
    };
    sim.store.keys.insert(other.dev_id, other.k_dev);
    sim.policy.refuse = true;
    let out = sim.client.connect(sim.now, other, sim.clock_us());
    for o in out {
        if let eternal_wire::v3::client::ClientOutput::Send(d) = o {
            sim.inject(Dir::ToHost, &d);
        }
    }
    assert!(sim.run_until(ms(1000), |s| s
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadRefused { .. }))));
    sim.policy.refuse = false;
    sim.run_for(Duration::from_secs(30));
    let live = sim.host_live().expect("reconnected under the first key");
    assert_ne!(live, first);
    assert!(sim.both_live_on_same_session());
}

/// The caller's decision on a confirm arrives 0 to 3 s late. A decision the
/// iPad has outwaited never commits a session it abandoned, and its old
/// session keeps running meanwhile.
#[test]
fn slow_decisions_never_cut_off_the_session_the_ipad_still_uses() {
    for delay_ms in [0u64, 500, 1500, 1900, 2500, 3000] {
        let mut sim = Sim::new(30 + delay_ms);
        let old = connect_live(&mut sim);
        sim.policy.decision_delay = ms(delay_ms);
        sim.connect();
        sim.run_for(Duration::from_secs(12));
        let ipad_live = live_ids(&sim);
        for event in &sim.events {
            if let Event::HostCommitted { session_id, .. } = event {
                assert!(
                    ipad_live.contains(session_id),
                    "delay {delay_ms} ms: committed {session_id}, which the iPad abandoned"
                );
            }
        }
        assert!(
            !sim.events.iter().any(|e| matches!(e,
                Event::HostClosed { session_id, reason: CloseReason::Liveness, .. } if *session_id == old)),
            "delay {delay_ms} ms"
        );
        assert!(sim.both_live_on_same_session(), "delay {delay_ms} ms");
        if delay_ms >= 2000 {
            // Every attempt is outwaited: the old session carries on.
            assert_eq!(sim.host_live(), Some(old), "delay {delay_ms} ms");
            assert!(sim
                .events
                .iter()
                .any(|e| matches!(e, Event::HostConfirmCancelled { .. })));
        } else {
            assert_ne!(sim.host_live(), Some(old), "delay {delay_ms} ms");
        }
    }
}

/// Refusal with the full invalidation sequence in between: prepare, then
/// step 1 ends the record's sessions, then the durable writes, then refuse.
#[test]
fn a_refusal_reaches_the_ipad_through_the_full_invalidation_sequence() {
    for (decision, durable) in [
        (0u64, 5u64),
        (200, 300),
        (900, 900),
        (1000, 1000),
        (500, 1600),
        (1200, 1500),
        (500, 3000),
        (0, 3000),
    ] {
        let mut sim = Sim::new(40 + decision);
        let live = connect_live(&mut sim);
        let other = ClientKey {
            dev_id: [0x5A; 16],
            k_dev: [0xA5; 32],
        };
        sim.store.keys.insert(other.dev_id, other.k_dev);
        sim.policy.refuse = true;
        sim.policy.invalidate_before_refuse = true;
        sim.policy.decision_delay = ms(decision);
        sim.policy.durable_delay = ms(durable);
        sim.connect_with(other.clone());
        assert!(
            sim.run_until(ms(6000), |s| s
                .events
                .iter()
                .any(|e| matches!(e, Event::IpadRefused { .. }))),
            "decision {decision} ms, durable {durable} ms"
        );
        let status = sim
            .events
            .iter()
            .find_map(|e| match e {
                Event::IpadRefused { status, .. } => Some(status.clone()),
                _ => None,
            })
            .unwrap();
        assert!(status.tombstones.contains(&other.dev_id));
        assert_eq!(sim.host_live(), Some(live));
        assert_eq!(sim.client.live_session(), Some(live));
    }
}

/// Reports keep the old session alive while two whole confirm windows are
/// lost; only input waits for the cutover.
#[test]
fn the_old_session_survives_two_lost_confirm_windows() {
    let mut sim = Sim::new(50);
    let old = connect_live(&mut sim);
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::Confirm if sent.nth < 40 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    assert!(
        sim.run_until(Duration::from_secs(8), |s| s.host_live() != Some(old)
            && s.both_live_on_same_session())
    );
    assert!(
        !sim.events.iter().any(|e| matches!(e,
        Event::HostClosed { session_id, reason: CloseReason::Liveness, .. } if *session_id == old))
    );
    let old_reports = sim
        .events
        .iter()
        .filter(|e| matches!(e, Event::HostDelivered { session_id, .. } if *session_id == old))
        .count();
    assert!(old_reports >= 8, "{old_reports} reports on the old session");
}

#[test]
fn a_delayed_older_record_from_the_old_address_never_moves_the_host_back() {
    let mut sim = Sim::new(51);
    sim.policy.media_every = None;
    connect_live(&mut sim);
    let held = sim
        .client
        .seal(sim.now, PacketType::ReceiverReport as u8, &[0; REPORT_BODY])
        .unwrap();
    let roamed = SimAddr {
        ip: 9,
        port: 40_000,
    };
    sim.roam(roamed);
    sim.run_for(ms(1200));
    let dev_id = sim.key.dev_id;
    assert_eq!(sim.host.seal(sim.now, &dev_id, 1, &[]).unwrap().0, roamed);
    sim.inject_from(IPAD, &held);
    assert_eq!(
        sim.host.seal(sim.now, &dev_id, 1, &[]).unwrap().0,
        roamed,
        "a delayed copy from the old address doesn't move the destination"
    );
    assert!(sim.both_live_on_same_session());
}

#[test]
fn refusal_resends_go_only_to_the_address_that_confirmed() {
    let mut sim = Sim::new(52);
    connect_live(&mut sim);
    let other = ClientKey {
        dev_id: [0x3C; 16],
        k_dev: [0xC3; 32],
    };
    sim.store.keys.insert(other.dev_id, other.k_dev);
    sim.policy.refuse = true;
    // The first refusal is lost; the iPad then moves and retries from there.
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::AckOrRefused if sent.nth == 1 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect_with(other);
    assert!(sim.run_until(ms(500), |s| s
        .sent_of(Dir::ToIpad, Kind::AckOrRefused)
        .len()
        == 2));
    let roamed = SimAddr { ip: 11, port: 1 };
    sim.roam(roamed);
    sim.run_for(ms(3000));
    let refusals = sim.sent_of(Dir::ToIpad, Kind::AckOrRefused);
    assert!(refusals.iter().skip(1).all(|r| r.to == IPAD));
    assert!(!sim
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadRefused { .. })));
}

#[test]
fn two_ipads_run_independently() {
    let mut sim = Sim::new(53);
    let second = sim.add_ipad(SimAddr {
        ip: 20,
        port: 50_000,
    });
    sim.connect();
    sim.connect_other(second);
    assert!(sim.run_until(ms(1000), |s| s.both_live_on_same_session()
        && s.other_live_on_same_session(second)));
    let first = sim.host_live().unwrap();
    let commits = sim.commits().len();
    // The second iPad reconnects, loses a confirm window, and is revoked.
    sim.fault = Box::new(|sent| match (sent.kind, sent.from.ip) {
        (Kind::Confirm, 20) if sent.nth < 25 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect_other(second);
    sim.run_for(ms(4000));
    let dev2 = sim.others[second].key.dev_id;
    let out = sim.host.revoke(sim.now, &dev2);
    assert!(!out.is_empty());
    sim.run_for(ms(2000));
    assert_eq!(sim.host_live(), Some(first), "the first iPad never noticed");
    assert_eq!(sim.commits().len(), commits);
    assert!(sim.both_live_on_same_session());
}

#[test]
fn a_flood_from_other_sources_never_blocks_the_ipad() {
    use eternal_wire::v3::handshake::Initiator;
    use eternal_wire::v3::wire::HelloPayload;
    let mut sim = Sim::new(54);
    let dev_id = sim.key.dev_id;
    let mut forged = Vec::new();
    for i in 0..50u16 {
        // The right dev_id with a wrong key, and unknown dev_ids.
        let wrong = Initiator::start(
            &[i as u8; 32],
            &dev_id,
            &HOST_ID,
            1,
            &HelloPayload::default(),
        )
        .unwrap();
        let unknown = Initiator::start(
            &[1; 32],
            &[i as u8; 16],
            &HOST_ID,
            1,
            &HelloPayload::default(),
        )
        .unwrap();
        forged.push(wrong.msg1().to_vec());
        forged.push(unknown.msg1().to_vec());
    }
    sim.connect();
    let mut i = 0usize;
    let done = sim.run_until(ms(1000), |s| s.both_live_on_same_session());
    assert!(done);
    // Again with the flood running every tick from 100 spoofed sources.
    let mut sim2 = Sim::new(55);
    sim2.connect();
    let mut live = false;
    for _ in 0..200 {
        for _ in 0..10 {
            let from = SimAddr {
                ip: 100 + (i % 100) as u16,
                port: i as u16,
            };
            sim2.inject_from(from, &forged[i % forged.len()]);
            i += 1;
        }
        sim2.step();
        if sim2.both_live_on_same_session() {
            live = true;
            break;
        }
    }
    assert!(live, "the iPad connects through the flood");
    assert_eq!(sim2.host.pending_count(), 0);
}

#[test]
fn the_pending_cap_holds_with_many_ipads_connecting() {
    let mut sim = Sim::new(56);
    let others: Vec<usize> = (0..17u16)
        .map(|i| {
            sim.add_ipad(SimAddr {
                ip: 200 + i,
                port: 1,
            })
        })
        .collect();
    // Their confirms are lost, so they hold pending sessions.
    sim.fault = Box::new(|sent| match (sent.kind, sent.from.ip) {
        (Kind::Confirm, ip) if ip >= 200 => Fate::Drop,
        _ => Fate::Deliver,
    });
    for &i in &others {
        sim.connect_other(i);
    }
    sim.run_for(ms(100));
    assert_eq!(sim.host.pending_count(), 16);
    sim.connect();
    sim.run_for(ms(100));
    assert!(sim.host.pending_session(&sim.key.dev_id).is_none());
    assert!(sim.host.pending_count() <= 16);
}

/// The caller's decision lands just as the iPad's confirm window runs out,
/// within one RTT of it, and the first copy of the next message 1 is lost.
/// The host may commit the abandoned attempt; the iPad must then use it.
#[test]
fn a_decision_landing_as_the_ipad_gives_up_still_reaches_it() {
    for latency_ms in [50u64, 100] {
        for decision_ms in [
            2000 - 2 * latency_ms,
            2000 - 3 * latency_ms / 2,
            2000 - latency_ms,
            2000 - latency_ms / 2,
            2000,
        ] {
            let mut sim = Sim::new(60 + latency_ms + decision_ms);
            sim.latency = ms(latency_ms);
            let old = connect_live(&mut sim);
            sim.policy.decision_delay = ms(decision_ms);
            // Message 1 of the attempt after the one being decided: lost once.
            sim.fault = Box::new(|sent| match sent.kind {
                Kind::Msg1 if sent.nth == 2 => Fate::Drop,
                _ => Fate::Deliver,
            });
            sim.connect();
            sim.run_for(Duration::from_secs(12));
            let label = format!("latency {latency_ms} ms, decision {decision_ms} ms");
            assert_eq!(
                sim.unused_commits(),
                0,
                "{label}: a commit the iPad never used"
            );
            assert!(sim.both_live_on_same_session(), "{label}");
            assert_ne!(sim.host_live(), Some(old), "{label}");
            sim.check_live_only_on_commits();
        }
    }
}

/// The same race, over and over: a reconnect every 7 s for 60 s.
#[test]
fn repeated_reconnects_with_late_decisions_never_waste_a_commit() {
    for latency_ms in [50u64, 100] {
        let mut sim = Sim::new(70 + latency_ms);
        sim.latency = ms(latency_ms);
        connect_live(&mut sim);
        sim.policy.decision_delay = ms(2000 - latency_ms);
        for _ in 0..8 {
            sim.connect();
            sim.run_for(Duration::from_secs(7));
        }
        assert_eq!(sim.unused_commits(), 0, "latency {latency_ms} ms");
        assert!(sim.both_live_on_same_session(), "latency {latency_ms} ms");
        assert!(!sim.events.iter().any(|e| matches!(
            e,
            Event::HostClosed {
                reason: CloseReason::Liveness,
                ..
            }
        )));
    }
}

#[test]
#[should_panic(expected = "never committed")]
fn the_live_check_covers_every_ipad() {
    let mut sim = Sim::new(95);
    let second = sim.add_ipad(SimAddr { ip: 30, port: 1 });
    sim.connect();
    sim.connect_other(second);
    sim.run_for(ms(500));
    sim.check_live_only_on_commits();
    // A live event for the second iPad on a session committed only for the
    // first must be caught.
    let first = sim.host_live().unwrap();
    sim.others[second].events.push(Event::IpadLive {
        at: sim.now,
        session_id: first,
    });
    sim.check_live_only_on_commits();
}

/// A refusal sealed late, after a long durable write, whose first copy is
/// lost: the scheduled resends still reach the iPad's retained key.
#[test]
fn a_late_refusal_with_its_first_copy_lost_still_arrives() {
    for (decision, durable) in [(1900u64, 4500u64), (1500, 4800), (1000, 1000), (0, 3000)] {
        let mut sim = Sim::new(100 + decision + durable);
        let live = connect_live(&mut sim);
        let other = ClientKey {
            dev_id: [0x4B; 16],
            k_dev: [0xB4; 32],
        };
        sim.store.keys.insert(other.dev_id, other.k_dev);
        sim.policy.refuse = true;
        sim.policy.decision_delay = ms(decision);
        sim.policy.durable_delay = ms(durable);
        // The first refusal is lost (nth 0 is the earlier ack).
        sim.fault = Box::new(|sent| match sent.kind {
            Kind::AckOrRefused if sent.nth == 1 => Fate::Drop,
            _ => Fate::Deliver,
        });
        sim.connect_with(other.clone());
        assert!(
            sim.run_until(Duration::from_secs(12), |s| s
                .events
                .iter()
                .any(|e| matches!(e, Event::IpadRefused { .. }))),
            "decision {decision} ms, durable {durable} ms"
        );
        assert!(sim.sent_of(Dir::ToIpad, Kind::AckOrRefused).len() >= 3);
        assert_eq!(sim.client.live_session(), Some(live));
        assert_eq!(sim.host_live(), Some(live));
    }
}

/// Sets up an attempt the host will commit just after the iPad gives up on
/// it, and runs until the iPad has abandoned it. Every later message 1 is
/// lost, so nothing else replaces it.
fn abandon_then_late_commit(seed: u64) -> Sim {
    let mut sim = Sim::new(seed);
    sim.latency = ms(100);
    connect_live(&mut sim);
    sim.policy.decision_delay = ms(1950);
    sim.fault = Box::new(|sent| match sent.kind {
        Kind::Msg1 if sent.nth >= 2 => Fate::Drop,
        _ => Fate::Deliver,
    });
    sim.connect();
    assert!(sim.run_until(ms(3000), |s| s.client.abandoned_count() > 0));
    sim
}

#[test]
fn after_disconnect_a_late_commit_never_brings_the_session_back() {
    let mut sim = abandon_then_late_commit(110);
    sim.disconnect();
    let ended = sim.now;
    sim.run_for(Duration::from_secs(8));
    assert!(sim.client.live_session().is_none());
    assert!(!sim
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadLive { at, .. } if *at > ended)));
    assert_eq!(sim.client.abandoned_count(), 0);
}

#[test]
fn a_new_connect_under_another_key_is_never_cancelled_by_an_older_attempt() {
    let mut sim = abandon_then_late_commit(111);
    let repaired = ClientKey {
        dev_id: [0x2E; 16],
        k_dev: [0xE2; 32],
    };
    sim.store.keys.insert(repaired.dev_id, repaired.k_dev);
    sim.fault = Box::new(|_| Fate::Deliver);
    sim.connect_with(repaired.clone());
    assert_eq!(
        sim.client.abandoned_count(),
        0,
        "other keys' attempts dropped"
    );
    assert!(sim.run_until(Duration::from_secs(5), |s| {
        let host = s.host.live_session(&repaired.dev_id);
        host.is_some() && host == s.client.live_session()
    }));
}

#[test]
fn a_refusal_under_an_abandoned_attempt_stops_using_that_key() {
    let mut sim = Sim::new(112);
    let live = connect_live(&mut sim);
    let other = ClientKey {
        dev_id: [0x6D; 16],
        k_dev: [0xD6; 32],
    };
    sim.store.keys.insert(other.dev_id, other.k_dev);
    sim.policy.refuse = true;
    sim.policy.decision_delay = ms(1000);
    sim.policy.durable_delay = ms(2500);
    sim.connect_with(other);
    assert!(sim.run_until(Duration::from_secs(8), |s| s
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadRefused { .. }))));
    let refused_at = sim.now;
    assert!(!sim.client.is_connecting() && !sim.client.is_confirming());
    sim.run_for(Duration::from_secs(5));
    assert!(
        !sim.events
            .iter()
            .any(|e| matches!(e, Event::IpadAttemptFailed { at } if *at >= refused_at)),
        "no AttemptFailed after pair again"
    );
    assert_eq!(sim.client.live_session(), Some(live));
}

#[test]
fn promoting_an_abandoned_attempt_releases_held_input() {
    let mut sim = abandon_then_late_commit(113);
    // Input pressed in the gap, on the old session, while the next attempt
    // is still connecting.
    let input = sim
        .client
        .seal(sim.now, PacketType::InputEvent as u8, &[0; 30]);
    assert!(input.is_some(), "input flows while only connecting");
    let pressed = sim.now;
    assert!(
        sim.run_until(Duration::from_secs(4), |s| s.client.abandoned_count() == 0
            && s.both_live_on_same_session())
    );
    let live_at = sim
        .events
        .iter()
        .filter_map(|e| match e {
            Event::IpadLive { at, .. } if *at > pressed => Some(*at),
            _ => None,
        })
        .next()
        .expect("the abandoned attempt went live");
    assert!(
        sim.events
            .iter()
            .any(|e| matches!(e, Event::IpadReleased { at } if *at == live_at)),
        "a release at the promotion"
    );
    assert_eq!(sim.unused_commits(), 0);
}

/// A key whose committed timestamp reaches the ceiling is done: the iPad is
/// told to pair again, and old message 1s never get back in.
#[test]
fn the_timestamp_ceiling_means_pair_again() {
    use eternal_wire::v3::MAX_TIMESTAMP_US;
    let mut sim = Sim::new(114);
    sim.clock_offset_us = (MAX_TIMESTAMP_US as i64) + 1_000;
    sim.client.set_last_timestamp(MAX_TIMESTAMP_US - 3);
    connect_live(&mut sim); // MAX - 2
    let first_msg1 = sim
        .sent_of(Dir::ToHost, Kind::Msg1)
        .last()
        .unwrap()
        .datagram
        .clone();
    connect_live(&mut sim); // MAX - 1, the ceiling
    let commits = sim.commits().len();
    sim.connect();
    assert!(sim.run_until(ms(1000), |s| s
        .events
        .iter()
        .any(|e| matches!(e, Event::IpadPairAgain { .. }))));
    // Replays of an older message 1 never commit either.
    for _ in 0..3 {
        sim.run_for(Duration::from_secs(35));
        sim.inject(Dir::ToHost, &first_msg1);
    }
    sim.run_for(ms(500));
    assert_eq!(sim.commits().len(), commits);
    assert!(sim.host.pending_session(&sim.key.dev_id).is_none());
}

/// An iPad clock far ahead of the host's gets TooNew, then connects with a
/// timestamp within the host's bound.
#[test]
fn a_clock_far_ahead_of_the_host_still_connects() {
    let mut sim = Sim::with_wall_clock(115);
    sim.clock_offset_us += 3 * 24 * 60 * 60 * 1_000_000;
    connect_live(&mut sim);
    assert_eq!(
        sim.sent_of(Dir::ToIpad, Kind::Msg2).len(),
        2,
        "TooNew, then Accept"
    );
    connect_live(&mut sim);
    assert_eq!(sim.commits().len(), 2);
}

/// Forged message 1s for the iPad's dev_id from more sources than the rate
/// table holds: the iPad reconnecting from its live address gets through on
/// its reserved budget. Unknown dev_ids cost nothing, so a fresh connect
/// gets through too.
#[test]
fn floods_never_block_a_reconnect_from_the_live_address() {
    use eternal_wire::v3::handshake::Initiator;
    use eternal_wire::v3::wire::HelloPayload;
    for known in [false, true] {
        let mut sim = Sim::new(116 + u64::from(known));
        if known {
            connect_live(&mut sim);
        }
        let dev_id = sim.key.dev_id;
        let junk: Vec<Vec<u8>> = (0..40u8)
            .map(|i| {
                let id = if known { dev_id } else { [i; 16] };
                Initiator::start(&[i; 32], &id, &HOST_ID, 1, &HelloPayload::default())
                    .unwrap()
                    .msg1()
                    .to_vec()
            })
            .collect();
        let mut n = 0usize;
        let mut flood = |sim: &mut Sim| {
            for _ in 0..60 {
                let source = SimAddr {
                    ip: 1000 + (n % 1100) as u16,
                    port: 1,
                };
                sim.inject_from(source, &junk[n % junk.len()]);
                n += 1;
            }
        };
        for _ in 0..40 {
            flood(&mut sim);
            sim.step();
        }
        let before = sim.host_live();
        sim.connect();
        let mut done = false;
        for _ in 0..600 {
            flood(&mut sim);
            sim.step();
            if sim.both_live_on_same_session() && sim.host_live() != before {
                done = true;
                break;
            }
        }
        assert!(done, "known dev_id: {known}");
    }
}
