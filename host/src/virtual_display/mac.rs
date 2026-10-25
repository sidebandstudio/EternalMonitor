//! `CGVirtualDisplay`, the private CoreGraphics API, made the way
//! Chromium's `virtual_display_mac_util.mm` and the phase 0 probe make it:
//! HiDPI on, modes at half the pixel size (points), `maxPixels` at the full
//! size, and a vendor ID and serial of its own (macOS 14 needs both).
//!
//! Every class and selector is checked at runtime before use, so a macOS
//! that drops or reshapes the API yields `DisplayError::Unavailable` and the
//! host falls back to mirroring, instead of crashing.
//!
//! Nothing in the test suite creates a display: the tests only look the
//! classes up.

use std::ffi::{c_uint, CStr};
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_core_foundation::CGSize;
use objc2_core_graphics::{CGDisplayBounds, CGDisplayIsOnline};
use objc2_foundation::{NSArray, NSString};

use super::{
    DisplayError, DisplaySpec, VirtualDisplay, VirtualDisplayManager, PRODUCT_ID, VENDOR_ID,
};
use crate::display_profile::{fallback_refresh, DisplayProfile};
use crate::input::mac::display::DisplayBounds;

/// How long a new display may take to come online, and a released one to
/// go away (the probe's limits).
const ONLINE_TIMEOUT: Duration = Duration::from_secs(3);
const REMOVE_TIMEOUT: Duration = Duration::from_secs(2);
/// About a phone panel's density. Only HiDPI mode selection depends on it;
/// spike c checks the resulting backing scale.
const PIXELS_PER_INCH: f64 = 413.0;

extern "C" {
    /// libdispatch. Global queues live for the process.
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut AnyObject;
}
/// `QOS_CLASS_USER_INITIATED`.
const QOS_USER_INITIATED: isize = 0x19;

/// The private classes, each checked for every selector the host sends.
struct Classes {
    descriptor: &'static AnyClass,
    settings: &'static AnyClass,
    mode: &'static AnyClass,
    display: &'static AnyClass,
}

fn class(name: &CStr, instance_selectors: &[Sel]) -> Result<&'static AnyClass, DisplayError> {
    let class = AnyClass::get(name)
        .ok_or_else(|| DisplayError::Unavailable(format!("no class {}", name.to_string_lossy())))?;
    for selector in instance_selectors {
        if class.instance_method(*selector).is_none() {
            return Err(DisplayError::Unavailable(format!(
                "{} has no {}",
                name.to_string_lossy(),
                selector.name().to_string_lossy()
            )));
        }
    }
    Ok(class)
}

impl Classes {
    fn load() -> Result<Self, DisplayError> {
        Ok(Self {
            descriptor: class(
                c"CGVirtualDisplayDescriptor",
                &[
                    sel!(setQueue:),
                    sel!(setName:),
                    sel!(setMaxPixelsWide:),
                    sel!(setMaxPixelsHigh:),
                    sel!(setSizeInMillimeters:),
                    sel!(setVendorID:),
                    sel!(setProductID:),
                    sel!(setSerialNum:),
                ],
            )?,
            settings: class(
                c"CGVirtualDisplaySettings",
                &[sel!(setModes:), sel!(setHiDPI:)],
            )?,
            mode: class(
                c"CGVirtualDisplayMode",
                &[sel!(initWithWidth:height:refreshRate:)],
            )?,
            display: class(
                c"CGVirtualDisplay",
                &[
                    sel!(initWithDescriptor:),
                    sel!(applySettings:),
                    sel!(displayID),
                ],
            )?,
        })
    }
}

/// Whether this macOS has the API in the shape the host uses. Creates
/// nothing.
pub fn available() -> Result<(), DisplayError> {
    Classes::load().map(|_| ())
}

/// How a mode change is made. macOS 26 reportedly cannot remove a display
/// whose mode was changed, so the default recreates it; spike c decides
/// whether switching in place is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeChange {
    Recreate,
    ApplySettings,
}

/// Displays owned by this process. They go away when it exits.
pub struct CgVirtualDisplays {
    classes: Classes,
    mode_change: ModeChange,
    /// Each display's ID, object and name.
    displays: Vec<(u32, Retained<AnyObject>, String)>,
}

impl CgVirtualDisplays {
    pub fn new(mode_change: ModeChange) -> Result<Self, DisplayError> {
        Ok(Self {
            classes: Classes::load()?,
            mode_change,
            displays: Vec::new(),
        })
    }

    fn settings_for(&self, profile: DisplayProfile) -> Result<Retained<AnyObject>, DisplayError> {
        let (width, height) = profile.points();
        let mut modes = Vec::new();
        for refresh in std::iter::once(profile.refresh_hz).chain(fallback_refresh(profile)) {
            // SAFETY: the class answers alloc, and the selector was checked.
            let mode: Option<Retained<AnyObject>> = unsafe {
                let allocated: Allocated<AnyObject> = msg_send![self.classes.mode, alloc];
                msg_send![
                    allocated,
                    initWithWidth: width as c_uint,
                    height: height as c_uint,
                    refreshRate: f64::from(refresh)
                ]
            };
            modes.push(mode.ok_or_else(|| {
                DisplayError::ModeRejected(format!("{width}x{height}@{refresh} refused"))
            })?);
        }
        let modes = NSArray::from_retained_slice(&modes);
        // SAFETY: as above; the setters take an NSArray and an unsigned int.
        unsafe {
            let settings: Option<Retained<AnyObject>> = msg_send![self.classes.settings, new];
            let settings =
                settings.ok_or_else(|| DisplayError::CreateFailed("no settings object".into()))?;
            let _: () = msg_send![&*settings, setModes: &*modes];
            let _: () = msg_send![&*settings, setHiDPI: c_uint::from(profile.hidpi)];
            Ok(settings)
        }
    }

    fn apply(&self, display: &AnyObject, profile: DisplayProfile) -> Result<(), DisplayError> {
        let settings = self.settings_for(profile)?;
        // SAFETY: a live CGVirtualDisplay; selector checked.
        let applied: bool = unsafe { msg_send![display, applySettings: &*settings] };
        if applied {
            Ok(())
        } else {
            Err(DisplayError::ModeRejected(format!("{profile:?}")))
        }
    }

    fn take(&mut self, id: u32) -> Option<(Retained<AnyObject>, String)> {
        let index = self
            .displays
            .iter()
            .position(|(online, ..)| *online == id)?;
        let (_, object, name) = self.displays.remove(index);
        Some((object, name))
    }
}

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl VirtualDisplayManager for CgVirtualDisplays {
    fn create(&mut self, spec: &DisplaySpec) -> Result<VirtualDisplay, DisplayError> {
        let profile = spec.profile;
        let millimetres = |pixels: u32| f64::from(pixels) / PIXELS_PER_INCH * 25.4;
        // SAFETY: every selector was checked in `Classes::load`; argument
        // types follow Chromium's declarations (NSString, unsigned int,
        // CGSize, dispatch_queue_t).
        let display: Option<Retained<AnyObject>> = unsafe {
            let descriptor: Option<Retained<AnyObject>> = msg_send![self.classes.descriptor, new];
            let descriptor = descriptor
                .ok_or_else(|| DisplayError::CreateFailed("no descriptor object".into()))?;
            let queue = dispatch_get_global_queue(QOS_USER_INITIATED, 0);
            let _: () = msg_send![&*descriptor, setQueue: queue];
            let _: () = msg_send![&*descriptor, setName: &*NSString::from_str(&spec.name)];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: profile.width_px as c_uint];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: profile.height_px as c_uint];
            let _: () = msg_send![
                &*descriptor,
                setSizeInMillimeters: CGSize {
                    width: millimetres(profile.width_px),
                    height: millimetres(profile.height_px),
                }
            ];
            let _: () = msg_send![&*descriptor, setVendorID: VENDOR_ID as c_uint];
            let _: () = msg_send![&*descriptor, setProductID: PRODUCT_ID as c_uint];
            let _: () = msg_send![&*descriptor, setSerialNum: spec.serial as c_uint];
            let allocated: Allocated<AnyObject> = msg_send![self.classes.display, alloc];
            msg_send![allocated, initWithDescriptor: &*descriptor]
        };
        let display =
            display.ok_or_else(|| DisplayError::CreateFailed(format!("{spec:?} was refused")))?;
        // SAFETY: a live CGVirtualDisplay; selector checked.
        let id: u32 = unsafe { msg_send![&*display, displayID] };
        // Dropping `display` on any failure below removes it again.
        self.apply(&display, profile)?;
        if !wait_until(ONLINE_TIMEOUT, || CGDisplayIsOnline(id)) {
            return Err(DisplayError::NotOnline(id));
        }
        self.displays.push((id, display, spec.name.clone()));
        tracing::info!(
            id,
            name = %spec.name,
            serial = spec.serial,
            ?profile,
            "Virtual display created"
        );
        Ok(VirtualDisplay {
            id,
            serial: spec.serial,
            profile,
            bounds: CGDisplayBounds(id).into(),
        })
    }

    fn change_mode(
        &mut self,
        display: &VirtualDisplay,
        profile: DisplayProfile,
    ) -> Result<VirtualDisplay, DisplayError> {
        let fits = profile.width_px <= display.profile.width_px
            && profile.height_px <= display.profile.height_px;
        if self.mode_change == ModeChange::ApplySettings && fits {
            let object = self
                .displays
                .iter()
                .find(|(id, ..)| *id == display.id)
                .map(|(_, object, _)| object.clone())
                .ok_or(DisplayError::NotOnline(display.id))?;
            self.apply(&object, profile)?;
            return Ok(VirtualDisplay {
                profile,
                bounds: CGDisplayBounds(display.id).into(),
                ..*display
            });
        }
        // `maxPixels` is fixed at creation, so a larger mode needs a new
        // display anyway. The same identity brings back its arrangement.
        let name = self
            .displays
            .iter()
            .find(|(id, ..)| *id == display.id)
            .map(|(.., name)| name.clone())
            .ok_or(DisplayError::NotOnline(display.id))?;
        self.remove(display)?;
        self.create(&DisplaySpec {
            name,
            serial: display.serial,
            profile,
        })
    }

    fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError> {
        let Some((object, _)) = self.take(display.id) else {
            return Ok(());
        };
        drop(object);
        let id = display.id;
        if wait_until(REMOVE_TIMEOUT, || !CGDisplayIsOnline(id)) {
            tracing::info!(id, "Virtual display removed");
            Ok(())
        } else {
            Err(DisplayError::StillOnline(id))
        }
    }

    fn bounds(&self, display: &VirtualDisplay) -> Option<DisplayBounds> {
        CGDisplayIsOnline(display.id).then(|| CGDisplayBounds(display.id).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_class_or_selector_is_reported_not_crashed_on() {
        assert!(matches!(
            class(c"EternalMonitorNoSuchClass", &[]),
            Err(DisplayError::Unavailable(why)) if why.contains("EternalMonitorNoSuchClass")
        ));
        assert!(matches!(
            class(c"NSObject", &[sel!(eternalMonitorNoSuchSelector)]),
            Err(DisplayError::Unavailable(why)) if why.contains("eternalMonitorNoSuchSelector")
        ));
        assert!(class(c"NSObject", &[sel!(description)]).is_ok());
    }

    #[test]
    fn this_macos_has_the_private_api_in_the_shape_the_host_uses() {
        // Looks the classes and selectors up; creates no display. macOS 14
        // and later all have them (the minimum the plan recommends).
        available().expect("CGVirtualDisplay API");
    }
}
