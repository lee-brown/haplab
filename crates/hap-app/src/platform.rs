//! Platform-specific shell helpers, file associations, and system integration.

/// Register HapLab as a handler for .mov files in the Windows registry (HKCU).
/// Does not require administrator privileges.
pub fn register_mov_association() -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| format!("Cannot determine exe path: {}", e))?;
        let exe_str = exe.to_string_lossy().to_string();
        let cmd_str = format!("\"{}\" \"%1\"", exe_str);

        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // 1. Register ProgID under HKCU\Software\Classes\HapLab.mov
        let _ = std::process::Command::new("reg")
            .args(&["add", r"HKCU\Software\Classes\HapLab.mov", "/ve", "/d", "HAP Video Movie", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // 2. Set default open command
        let _ = std::process::Command::new("reg")
            .args(&["add", r"HKCU\Software\Classes\HapLab.mov\shell\open\command", "/ve", "/d", &cmd_str, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // 3. Register .mov association in OpenWithProgids
        let _ = std::process::Command::new("reg")
            .args(&["add", r"HKCU\Software\Classes\.mov\OpenWithProgids", "/v", "HapLab.mov", "/t", "REG_NONE", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // 4. Register in Applications
        let _ = std::process::Command::new("reg")
            .args(&["add", r"HKCU\Software\Classes\Applications\haplab.exe\shell\open\command", "/ve", "/d", &cmd_str, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // 5. Notify the Windows Shell that file associations have changed
        unsafe {
            extern "system" {
                fn SHChangeNotify(
                    wEventId: i32,
                    uFlags: u32,
                    dwItem1: *const std::ffi::c_void,
                    dwItem2: *const std::ffi::c_void,
                );
            }
            const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
            const SHCNF_IDLIST: u32 = 0;
            SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, std::ptr::null(), std::ptr::null());
        }

        Ok("HapLab registered for .mov files in Windows Explorer.".into())
    }

    #[cfg(not(windows))]
    {
        Err("File association registration is currently only implemented for Windows.".into())
    }
}

/// Open the native Windows Default Apps settings page.
pub fn open_windows_default_apps() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg("ms-settings:defaultapps")
            .spawn();
    }
}
