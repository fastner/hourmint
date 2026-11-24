//! Launch at login via SMAppService (macOS 13+). Works in the App Store sandbox,
//! and the user can also see and change it in System Settings → General → Login Items.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::{msg_send, rc::Retained};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    const fn four_cc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        unsafe {
            let service = SMAppService::mainAppService();
            let result = if enabled {
                service.registerAndReturnError()
            } else {
                service.unregisterAndReturnError()
            };
            result.map_err(|e| e.localizedDescription().to_string())
        }
    }

    /// Enabled, or waiting for the user to approve it in System Settings.
    pub fn is_enabled() -> bool {
        let status = unsafe { SMAppService::mainAppService().status() };
        status == SMAppServiceStatus::Enabled || status == SMAppServiceStatus::RequiresApproval
    }

    /// True when macOS opened the app as a login item. Must be called while the launch
    /// event is being handled (during app setup).
    pub fn launched_at_login() -> bool {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        let Some(event) = manager.currentAppleEvent() else {
            return false;
        };
        // keyAEPropData → keyAELaunchedAsLogInItem (same check as Apple's login item samples).
        let prop: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![&*event, paramDescriptorForKeyword: four_cc(b"prdt")] };
        let Some(prop) = prop else { return false };
        prop.enumCodeValue() == four_cc(b"lgit")
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("Launch at login is only available on macOS.".into())
    }
    pub fn is_enabled() -> bool {
        false
    }
    pub fn launched_at_login() -> bool {
        false
    }
}

pub use imp::{is_enabled, launched_at_login, set};

/// Builds before 0.1.1 used a LaunchAgent plist; remove it so the app isn't started twice.
#[cfg(all(target_os = "macos", feature = "direct"))]
pub fn remove_legacy_launch_agent() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let plist = std::path::Path::new(&home).join("Library/LaunchAgents/Hourmint.plist");
    plist.exists() && std::fs::remove_file(plist).is_ok()
}
