//! Windows registration for the command-side half of the native install.
//!
//! Astrolabe carries `lait.exe` in `current/`, but editor bindings deliberately
//! say only `lait mcp`: they are portable files, sometimes checked into a
//! shared project, and an absolute per-user install path would make them work
//! on exactly one machine. The per-user installer therefore prepends that
//! stable `current/` coordinate to the per-user PATH and removes only its own
//! entry on uninstall.
//!
//! This is native code rather than an NSIS string macro. Ordinary NSIS strings
//! cap at 1024 characters; reading a longer PATH looks the same as a missing
//! value to the common `EnvVarUpdate` macro and can overwrite it. Win32's
//! size-query/read/write sequence preserves the complete value instead.

use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_VALUE_TYPE, RRF_NOEXPAND,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

use crate::CURRENT_DIR;

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the successful result of `RegOpenKeyExW` and is
        // closed exactly once here.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn status(error: u32) -> String {
    std::io::Error::from_raw_os_error(error.cast_signed()).to_string()
}

fn environment_key() -> Result<Key, String> {
    let environment = wide("Environment");
    let mut key = std::ptr::null_mut();
    // SAFETY: the string is NUL-terminated, `key` is a valid out pointer, and
    // the returned handle is owned by `Key`.
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            environment.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &raw mut key,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(format!("open HKCU\\Environment: {}", status(result)));
    }
    Ok(Key(key))
}

fn read(key: &Key) -> Result<(String, REG_VALUE_TYPE), String> {
    let name = wide("Path");
    let flags = RRF_NOEXPAND | RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut kind = REG_EXPAND_SZ;
    let mut bytes = 0u32;
    // SAFETY: this is the documented size query: both data pointers are null
    // and `bytes` receives the required size.
    let result = unsafe {
        RegGetValueW(
            key.0,
            std::ptr::null(),
            name.as_ptr(),
            flags,
            &raw mut kind,
            std::ptr::null_mut(),
            &raw mut bytes,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok((String::new(), REG_EXPAND_SZ));
    }
    if result != ERROR_SUCCESS {
        return Err(format!("read the user Path size: {}", status(result)));
    }

    loop {
        let units = usize::try_from(bytes)
            .ok()
            .and_then(|value| value.checked_add(1))
            .map(|value| value / 2)
            .ok_or_else(|| "the user Path is too large to address".to_owned())?;
        let mut value = vec![0u16; units.max(1)];
        let mut available = u32::try_from(value.len() * 2)
            .map_err(|_| "the user Path is too large to address".to_owned())?;
        // SAFETY: `value` owns `available` writable bytes and the other
        // pointers remain valid for the call.
        let result = unsafe {
            RegGetValueW(
                key.0,
                std::ptr::null(),
                name.as_ptr(),
                flags,
                &raw mut kind,
                value.as_mut_ptr().cast(),
                &raw mut available,
            )
        };
        if result == ERROR_MORE_DATA {
            bytes = available;
            continue;
        }
        if result != ERROR_SUCCESS {
            return Err(format!("read the user Path: {}", status(result)));
        }
        let mut units = usize::try_from(available / 2)
            .map_err(|_| "the user Path is too large to address".to_owned())?;
        if units > 0 && value[units - 1] == 0 {
            units -= 1;
        }
        if value[..units].contains(&0) {
            return Err("the user Path contains an embedded NUL; it was left untouched".to_owned());
        }
        return String::from_utf16(&value[..units])
            .map(|value| (value, kind))
            .map_err(|_| "the user Path is not valid UTF-16; it was left untouched".to_owned());
    }
}

fn comparable(entry: &str) -> String {
    entry
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

fn updated(existing: &str, ours: &str, present: bool) -> Result<String, String> {
    if ours.contains(';') {
        return Err(
            "the Astrolabe install path contains ';' and cannot be represented on PATH".into(),
        );
    }
    let ours_key = comparable(ours);
    let mut entries: Vec<&str> = if existing.is_empty() {
        Vec::new()
    } else {
        existing
            .split(';')
            .filter(|entry| comparable(entry) != ours_key)
            .collect()
    };
    if present {
        entries.insert(0, ours);
    }
    Ok(entries.join(";"))
}

fn write(key: &Key, value: &str, kind: REG_VALUE_TYPE) -> Result<(), String> {
    let name = wide("Path");
    let value = wide(value);
    let bytes = value
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| "the updated user Path is too large to write".to_owned())?;
    // SAFETY: both strings are NUL-terminated; `value` owns exactly `bytes`
    // readable bytes and the key was opened with `KEY_SET_VALUE`.
    let result =
        unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, kind, value.as_ptr().cast(), bytes) };
    if result != ERROR_SUCCESS {
        return Err(format!("write the user Path: {}", status(result)));
    }
    Ok(())
}

fn broadcast() {
    let environment = wide("Environment");
    let mut delivered = 0usize;
    // SAFETY: `environment` remains alive and NUL-terminated for the
    // synchronous call. The registry write is already durable; a hung window
    // may time out, which must not roll the install back after the fact.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5_000,
            &raw mut delivered,
        );
    }
}

/// Add or remove the native install's stable command directory in the current
/// user's PATH. Repeated calls are idempotent and remove duplicate entries for
/// this exact installation without disturbing any other path.
pub fn set(root: &Path, present: bool) -> Result<(), String> {
    let ours = root
        .join(CURRENT_DIR)
        .to_str()
        .ok_or_else(|| "the Astrolabe install path is not valid Unicode".to_owned())?
        .to_owned();
    let key = environment_key()?;
    let (existing, kind) = read(&key)?;
    let next = updated(&existing, &ours, present)?;
    if next != existing {
        write(&key, &next, kind)?;
        broadcast();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_command_path_is_preferred_idempotent_and_removed_without_collateral_damage() {
        let cargo = r"C:\Users\Ada\.cargo\bin";
        let ours = r"C:\Users\Ada\AppData\Local\Programs\Astrolabe\current";
        let original = format!(r"{cargo};C:\Windows\System32");
        let installed = updated(&original, ours, true).expect("install path");
        assert_eq!(installed, format!(r"{ours};{original}"));
        assert_eq!(
            updated(&installed, ours, true).expect("reinstall path"),
            installed,
            "reinstalling duplicated the command path"
        );
        assert_eq!(
            updated(
                &format!(r"{installed};C:/USERS/ADA/APPDATA/LOCAL/PROGRAMS/ASTROLABE/CURRENT/"),
                ours,
                false
            )
            .expect("uninstall path"),
            original,
            "uninstall did not remove every equivalent entry or changed somebody else's path"
        );
        assert!(
            updated("unchanged", r"C:\semi;colon", true).is_err(),
            "a path-list delimiter inside the install path was accepted"
        );
    }
}
