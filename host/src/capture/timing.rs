//! Pacing for the synthetic source. macOS's ordinary sleep can coalesce
//! timers by several milliseconds, but spinning for a whole frame consumes
//! a core the software encoder and simulator need. A critical kqueue timer
//! sleeps until the final 500 us, which is the only busy-wait interval.

use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[cfg(target_os = "macos")]
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32;
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> i32;
}

pub(crate) struct FrameTimer {
    #[cfg(target_os = "macos")]
    queue: Option<OwnedFd>,
    #[cfg(target_os = "macos")]
    timebase: Timebase,
}

impl FrameTimer {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            // The synthetic capture thread produces frames for a live viewer.
            // This is a QoS hint, not a real-time scheduling reservation.
            unsafe {
                pthread_set_qos_class_self_np(0x21, 0);
            }
            let mut timebase = Timebase { numer: 0, denom: 0 };
            let fd = unsafe { libc::kqueue() };
            let queue = if fd >= 0 {
                Some(unsafe { OwnedFd::from_raw_fd(fd) })
            } else {
                None
            };
            if unsafe { mach_timebase_info(&mut timebase) } != 0 || timebase.numer == 0 {
                return Self {
                    queue: None,
                    timebase,
                };
            }
            Self { queue, timebase }
        }
        #[cfg(not(target_os = "macos"))]
        Self {}
    }

    pub fn wait_until(&mut self, deadline: Instant) {
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            #[cfg(target_os = "macos")]
            if let Some(queue) = &self.queue {
                let margin = Duration::from_micros(500);
                if remaining > margin {
                    let ticks = (remaining - margin).as_nanos() * u128::from(self.timebase.denom)
                        / u128::from(self.timebase.numer);
                    let at = unsafe { mach_absolute_time() }.saturating_add(ticks as u64);
                    let change = libc::kevent {
                        ident: 1,
                        filter: libc::EVFILT_TIMER,
                        flags: libc::EV_ADD | libc::EV_ONESHOT,
                        fflags: libc::NOTE_ABSOLUTE | libc::NOTE_MACHTIME | libc::NOTE_CRITICAL,
                        data: at as isize,
                        udata: std::ptr::null_mut(),
                    };
                    let mut event = change;
                    let result = unsafe {
                        libc::kevent(
                            queue.as_raw_fd(),
                            &change,
                            1,
                            &mut event,
                            1,
                            std::ptr::null(),
                        )
                    };
                    if result == 1 && event.flags & libc::EV_ERROR == 0 {
                        continue;
                    }
                    if result == -1
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    tracing::warn!("Synthetic capture timer unavailable; using spin pacing");
                    self.queue = None;
                }
            }
            if remaining > Duration::from_millis(150) {
                std::thread::sleep(Duration::from_millis(5));
            } else {
                std::hint::spin_loop();
            }
        }
    }
}
