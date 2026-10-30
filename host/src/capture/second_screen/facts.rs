//! The graphics cards and screens as Windows last reported them, for the
//! decisions that ask whether the mirror is blocked
//! ([`super::mirror_blocked_now`]).
//!
//! Reading them creates a DXGI factory, lists the adapters, asks DXCore and
//! reads the display configuration: driver calls with no limit of their own.
//! The transport loop decides on every pass, and must never wait for one, so
//! decisions read only the snapshot kept here, under no other lock. A refresh
//! runs on a thread of its own, one at a time, and reads under #110's limit
//! ([`Call::GpuFacts`]): a read past it is abandoned, not joined, and while it
//! still runs the next read is refused. A snapshot that is missing, older than
//! [`FRESH_FOR`], or from a read given up on says nothing, and decisions go as
//! on any PC where the mirror works.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::gpu_preference::GpuFacts;
use crate::stall::{Call, Lanes, Limits};

/// How long a snapshot counts, from when its read began. Longer than
/// [`REFRESH_EVERY`] plus a read's limit, so a PC whose reads return always
/// has a fresh one while a refusal is recorded.
pub(crate) const FRESH_FOR: Duration = Duration::from_secs(10);

/// While a refusal of the mirror is recorded, how often the transport's
/// passes start a refresh.
pub(crate) const REFRESH_EVERY: Duration = Duration::from_secs(3);

/// What a refresh reads, in which lane and under which limits.
#[derive(Clone)]
pub(crate) struct Reader {
    read: Arc<dyn Fn() -> Option<GpuFacts> + Send + Sync>,
    lanes: &'static Lanes,
    limits: Limits,
}

impl Reader {
    /// Windows' own, in the process's lane.
    fn windows() -> Self {
        Self {
            read: Arc::new(crate::gpu_preference::facts),
            lanes: crate::stall::lanes(),
            limits: Call::GpuFacts.limits(),
        }
    }

    /// `read` in place of Windows, for a test or the fake display.
    #[cfg(debug_assertions)]
    pub(crate) fn new(
        read: impl Fn() -> Option<GpuFacts> + Send + Sync + 'static,
        lanes: &'static Lanes,
        limits: Limits,
    ) -> Self {
        Self {
            read: Arc::new(read),
            lanes,
            limits,
        }
    }

    /// The facts, or `None` when Windows couldn't say, the read did not
    /// return within its limit, or an earlier one is still stuck.
    fn read(&self) -> Option<GpuFacts> {
        let read = self.read.clone();
        crate::stall::run_on(
            self.lanes,
            Call::GpuFacts,
            self.limits,
            move || read(),
            || {},
        )
        .ok()
        .flatten()
    }
}

/// The snapshot and its refresh. Clones share them.
#[derive(Clone, Default)]
pub(crate) struct Cache(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// When the last read that ended began, and what it found.
    snapshot: Option<(Instant, Option<GpuFacts>)>,
    refreshing: bool,
    /// When the last refresh began.
    last: Option<Instant>,
    #[cfg(debug_assertions)]
    fake: Option<Reader>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.0.lock();
        f.debug_struct("Cache")
            .field("snapshot", &inner.snapshot)
            .field("refreshing", &inner.refreshing)
            .finish_non_exhaustive()
    }
}

impl Inner {
    fn reader(&self) -> Option<Reader> {
        #[cfg(debug_assertions)]
        if let Some(fake) = &self.fake {
            return Some(fake.clone());
        }
        // Unit tests never read the PC's own.
        (!cfg!(test)).then(Reader::windows)
    }
}

impl Cache {
    /// The facts of a fresh snapshot, if it has them. Never reads Windows.
    pub(crate) fn fresh(&self) -> Option<GpuFacts> {
        let inner = self.0.lock();
        let (at, facts) = inner.snapshot.as_ref()?;
        if at.elapsed() >= FRESH_FOR {
            return None;
        }
        facts.clone()
    }

    /// Start a refresh, unless one runs or, when not `urgent`, the last
    /// began within [`REFRESH_EVERY`]. Never waits for it.
    pub(crate) fn refresh(&self, urgent: bool) {
        let now = Instant::now();
        let reader = {
            let mut inner = self.0.lock();
            let recent = inner
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < REFRESH_EVERY);
            if inner.refreshing || (recent && !urgent) {
                return;
            }
            let Some(reader) = inner.reader() else {
                return;
            };
            inner.refreshing = true;
            inner.last = Some(now);
            reader
        };
        let refresh = Refresh {
            cache: self.clone(),
            began: now,
            found: None,
        };
        // A thread that can't start drops the closure, and with it the guard.
        let _ = std::thread::Builder::new()
            .name("gpu-facts".into())
            .spawn(move || {
                let mut refresh = refresh;
                refresh.found = Some(reader.read());
            });
    }

    /// Have refreshes read `reader` in place of Windows.
    #[cfg(debug_assertions)]
    pub(crate) fn use_reader(&self, reader: Reader) {
        self.0.lock().fake = Some(reader);
    }

    /// A snapshot as if a read that began `at` found `facts`.
    #[cfg(test)]
    pub(crate) fn set(&self, at: Instant, facts: Option<GpuFacts>) {
        self.0.lock().snapshot = Some((at, facts));
    }
}

/// A refresh in flight. Dropped, it ends, and keeps what it found unless a
/// later read's snapshot is already there, so a thread that panics or
/// could not start loses only that refresh.
struct Refresh {
    cache: Cache,
    began: Instant,
    found: Option<Option<GpuFacts>>,
}

impl Drop for Refresh {
    fn drop(&mut self) {
        let mut inner = self.cache.0.lock();
        inner.refreshing = false;
        let newer = inner
            .snapshot
            .as_ref()
            .is_some_and(|(at, _)| *at > self.began);
        if let Some(found) = self.found.take().filter(|_| !newer) {
            inner.snapshot = Some((self.began, found));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// Short limits, so a read that never returns is given up on quickly.
    pub(crate) const QUICK: Limits = Limits {
        warn: Duration::from_millis(50),
        limit: Duration::from_millis(200),
    };

    /// No read is given up on while a test runs.
    pub(crate) const PATIENT: Limits = Limits {
        warn: Duration::from_secs(60),
        limit: Duration::from_secs(600),
    };

    /// A read of `facts` that blocks until the returned sender is dropped,
    /// in a lane of its own under `limits`, and how many reads began.
    pub(crate) fn stuck_reader(
        facts: Option<GpuFacts>,
        limits: Limits,
    ) -> (Reader, Arc<AtomicUsize>, std::sync::mpsc::Sender<()>) {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let reader = Reader::new(
            move || {
                counted.fetch_add(1, SeqCst);
                let _ = gate.lock().recv();
                facts.clone()
            },
            Box::leak(Box::new(Lanes::new())),
            limits,
        );
        (reader, reads, release)
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn some_facts() -> GpuFacts {
        crate::capture::tests::hybrid_laptop(true)
    }

    /// A snapshot counts for [`FRESH_FOR`] from when its read began.
    #[test]
    fn a_snapshot_counts_only_while_fresh() {
        let cache = Cache::default();
        assert_eq!(cache.fresh(), None, "none yet");
        cache.set(Instant::now(), Some(some_facts()));
        assert_eq!(cache.fresh(), Some(some_facts()));
        let old = Instant::now().checked_sub(FRESH_FOR).unwrap();
        cache.set(old, Some(some_facts()));
        assert_eq!(cache.fresh(), None, "stale");
    }

    /// Review round 2: a read that never returns. However often a refresh
    /// is asked for, one read runs; past its limit it is given up on, and
    /// its snapshot says nothing, though the one before it was fresh. The
    /// next refresh is refused while the stuck read still runs.
    #[test]
    fn a_read_that_never_returns_runs_alone_and_says_nothing() {
        let cache = Cache::default();
        let (reader, reads, release) = stuck_reader(Some(some_facts()), QUICK);
        cache.use_reader(reader);
        cache.set(Instant::now(), Some(some_facts()));
        for _ in 0..20 {
            cache.refresh(true);
        }
        wait_until("a read began", || reads.load(SeqCst) == 1);
        wait_until("the read is given up on", || cache.fresh().is_none());
        wait_until("the refresh ended", || !cache.0.lock().refreshing);
        for _ in 0..5 {
            cache.refresh(true);
            wait_until("the refresh ended", || !cache.0.lock().refreshing);
        }
        assert_eq!(reads.load(SeqCst), 1, "no read behind the stuck one");
        assert_eq!(cache.fresh(), None);
        // Once it returns, the lane frees and reads work again.
        drop(release);
        let lanes = cache.0.lock().fake.as_ref().unwrap().lanes;
        wait_until("the lane frees", || lanes.abandoned(Call::GpuFacts) == 0);
        cache.refresh(true);
        wait_until("a fresh snapshot", || cache.fresh().is_some());
        assert_eq!(reads.load(SeqCst), 2);
    }

    /// Without `urgent`, refreshes are at least [`REFRESH_EVERY`] apart.
    #[test]
    fn refreshes_are_spaced_unless_urgent() {
        let cache = Cache::default();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        cache.use_reader(Reader::new(
            move || {
                counted.fetch_add(1, SeqCst);
                Some(some_facts())
            },
            Box::leak(Box::new(Lanes::new())),
            QUICK,
        ));
        cache.refresh(false);
        wait_until("a fresh snapshot", || cache.fresh().is_some());
        wait_until("the refresh ended", || !cache.0.lock().refreshing);
        cache.refresh(false);
        assert_eq!(reads.load(SeqCst), 1, "spaced");
        cache.refresh(true);
        wait_until("an urgent read", || reads.load(SeqCst) == 2);
    }
}
