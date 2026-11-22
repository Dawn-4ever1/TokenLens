//! Per-installation pairing state and conservative laptop/desktop detection.
//! No host name, hardware UUID, serial number or other fingerprint is stored
//! or uploaded. The random request id only makes one pairing retry idempotent.

use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use usage_event::envelope::FormFactor;

const BINDING_FILE: &str = "account-device-binding";

/// A new random id is created before a server-side device exists. Save this
/// first, otherwise a lost HTTP response could create a duplicate on retry.
pub fn client_binding_id(state_dir: &Path) -> std::io::Result<String> {
    let path = state_dir.join(BINDING_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if valid_binding_id(id) {
            return Ok(id.to_owned());
        }
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    let id = URL_SAFE_NO_PAD.encode(bytes);
    std::fs::create_dir_all(state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, format!("{id}\n").as_bytes())
            })?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

fn valid_binding_id(id: &str) -> bool {
    (16..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(target_os = "macos")]
pub fn detected_form_factor() -> FormFactor {
    // Current macOS versions expose an internal family id (e.g. Mac18,5)
    // through ioreg, not necessarily the retail "Mac mini" product name.
    // system_profiler's Model Name is only the family, never the user-chosen
    // ComputerName, serial number or IOPlatformUUID.
    if let Ok(output) = std::process::Command::new("/usr/sbin/system_profiler")
        .arg("SPHardwareDataType")
        .output()
        && output.status.success()
        && let Some(name) = mac_model_name(&String::from_utf8_lossy(&output.stdout))
    {
        return form_factor_from_mac_model(name);
    }
    FormFactor::Laptop
}

fn mac_model_name(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Model Name:")
            .map(str::trim)
            .filter(|name| !name.is_empty())
    })
}

#[cfg(windows)]
#[repr(C)]
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetSystemPowerStatus(lp_system_power_status: *mut SystemPowerStatus) -> i32;
}

#[cfg(windows)]
pub fn detected_form_factor() -> FormFactor {
    let mut status = SystemPowerStatus {
        ac_line_status: 255,
        battery_flag: 255,
        battery_life_percent: 255,
        system_status_flag: 0,
        battery_life_time: u32::MAX,
        battery_full_life_time: u32::MAX,
    };
    // SAFETY: `status` is a valid aligned `SYSTEM_POWER_STATUS` out-parameter.
    let ok = unsafe { GetSystemPowerStatus(&raw mut status) };
    if ok != 0 {
        form_factor_from_windows_battery_flag(status.battery_flag)
    } else {
        FormFactor::Laptop
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn detected_form_factor() -> FormFactor {
    FormFactor::Laptop
}

/// Win32 `SYSTEM_POWER_STATUS.BatteryFlag`: `0x80` (128) means no system
/// battery is present (desktop PC); `0xFF` (255) means unknown status.
#[cfg(any(windows, test))]
pub fn form_factor_from_windows_battery_flag(battery_flag: u8) -> FormFactor {
    if battery_flag != 255 && (battery_flag & 128) != 0 {
        FormFactor::Desktop
    } else {
        FormFactor::Laptop
    }
}

pub fn form_factor_from_mac_model(model: &str) -> FormFactor {
    // The model string is a product family (e.g. "Mac15,3"), not a serial.
    // MacBook laptops are named explicitly. iMac, mini, Studio, Pro and
    // Ultra desktop families do not contain that product prefix.
    if model.contains("MacBook") {
        FormFactor::Laptop
    } else if model.contains("Mac")
        && [
            "iMac",
            "Mac mini",
            "Macmini",
            "MacStudio",
            "Mac Studio",
            "MacPro",
            "Mac Pro",
        ]
        .iter()
        .any(|family| model.contains(family))
    {
        FormFactor::Desktop
    } else {
        // Unknown/new Apple model: retain the original choice rather than
        // pretending the hardware was classified with certainty.
        FormFactor::Laptop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_product_families_distinguish_laptops_and_desktops() {
        for model in ["MacBookPro18,3", "MacBookAir15,2", "MacBook16,1"] {
            assert_eq!(form_factor_from_mac_model(model), FormFactor::Laptop);
        }
        for model in [
            "Mac16,10 Mac mini",
            "iMac21,1",
            "MacStudio14,2",
            "MacPro7,1",
        ] {
            assert_eq!(form_factor_from_mac_model(model), FormFactor::Desktop);
        }
        assert_eq!(
            form_factor_from_mac_model("MacFuture1,1"),
            FormFactor::Laptop
        );
        assert_eq!(
            mac_model_name(" Model Identifier: Mac18,5\n Model Name: Mac mini\n"),
            Some("Mac mini")
        );
    }

    #[test]
    fn saved_binding_is_stable_and_malformed_values_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let first = client_binding_id(dir.path()).unwrap();
        assert!(valid_binding_id(&first));
        assert_eq!(client_binding_id(dir.path()).unwrap(), first);
        std::fs::write(dir.path().join(BINDING_FILE), "not-a-real-id").unwrap();
        let repaired = client_binding_id(dir.path()).unwrap();
        assert!(valid_binding_id(&repaired));
        assert_ne!(repaired, "not-a-real-id");
    }

    #[test]
    fn windows_battery_flags_distinguish_desktops_and_laptops() {
        assert_eq!(
            form_factor_from_windows_battery_flag(128),
            FormFactor::Desktop
        );
        assert_eq!(form_factor_from_windows_battery_flag(1), FormFactor::Laptop);
        assert_eq!(form_factor_from_windows_battery_flag(8), FormFactor::Laptop);
        assert_eq!(form_factor_from_windows_battery_flag(0), FormFactor::Laptop);
        assert_eq!(
            form_factor_from_windows_battery_flag(255),
            FormFactor::Laptop
        );
    }
}
