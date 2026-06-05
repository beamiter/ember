// Cross-platform compatibility shims. On non-Windows builds the stub
// implementations and PlatformCapabilities are compiled but unused.
#![allow(dead_code)]

#[cfg(target_os = "windows")]
pub mod windows_support {
    use std::path::PathBuf;

    /// Windows ConPTY 增强支持
    pub struct ConPtyEnhanced {
        pub enable_vt_processing: bool,
        pub legacy_mode: bool,
    }

    impl ConPtyEnhanced {
        pub fn new() -> Self {
            ConPtyEnhanced {
                enable_vt_processing: true,
                legacy_mode: false,
            }
        }

        /// 启用 Windows 虚拟终端处理
        pub fn enable_vt_sequences() -> std::io::Result<()> {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;

            #[allow(non_camel_case_types)]
            type HANDLE = *mut std::ffi::c_void;
            const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
            const STD_INPUT_HANDLE: u32 = -10i32 as u32;
            const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

            extern "system" {
                fn GetStdHandle(nStdHandle: u32) -> HANDLE;
                fn SetConsoleMode(hConsoleHandle: HANDLE, dwMode: u32) -> i32;
                fn GetConsoleMode(hConsoleHandle: HANDLE, lpMode: *mut u32) -> i32;
            }

            unsafe {
                let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
                let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);

                if stdout_handle == std::ptr::null_mut() {
                    return Err(std::io::Error::last_os_error());
                }

                let mut mode = 0u32;
                if GetConsoleMode(stdout_handle, &mut mode) == 0 {
                    return Err(std::io::Error::last_os_error());
                }

                mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                if SetConsoleMode(stdout_handle, mode) == 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // 同时设置 stdin
                let mut input_mode = 0u32;
                if GetConsoleMode(stdin_handle, &mut input_mode) != 0 {
                    let _ = SetConsoleMode(stdin_handle, input_mode);
                }
            }

            Ok(())
        }

        /// 获取 Windows 终端路径
        pub fn get_terminal_path() -> Option<PathBuf> {
            // Windows Terminal 通常在这些位置
            let paths = vec![
                PathBuf::from(
                    "C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_*\\wt.exe",
                ),
                PathBuf::from("C:\\Program Files\\Windows Terminal\\wt.exe"),
                PathBuf::from(&std::env::var("LOCALAPPDATA").unwrap_or_default())
                    .join("Microsoft\\WindowsApps\\wt.exe"),
            ];

            for path in paths {
                if path.exists() {
                    return Some(path);
                }
            }

            None
        }

        /// 检测 Windows 版本
        pub fn get_windows_version() -> Option<String> {
            use std::process::Command;

            let output = Command::new("cmd").args(&["/C", "ver"]).output().ok()?;

            String::from_utf8(output.stdout).ok()
        }

        /// Windows 特定的路径处理
        pub fn normalize_path(path: &str) -> String {
            path.replace('/', "\\")
        }

        /// 检测是否在 Windows Terminal 中运行
        pub fn is_windows_terminal() -> bool {
            std::env::var("WT_SESSION").is_ok() || std::env::var("WT_PROFILE_ID").is_ok()
        }

        /// 检测是否在 WSL2 中运行
        pub fn is_wsl2() -> bool {
            if let Ok(wsl_distro) = std::env::var("WSL_DISTRO_NAME") {
                !wsl_distro.is_empty()
            } else {
                false
            }
        }
    }

    impl Default for ConPtyEnhanced {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub mod windows_support {
    use std::path::PathBuf;

    pub struct ConPtyEnhanced {
        pub enable_vt_processing: bool,
        pub legacy_mode: bool,
    }

    impl ConPtyEnhanced {
        pub fn new() -> Self {
            ConPtyEnhanced {
                enable_vt_processing: true,
                legacy_mode: false,
            }
        }

        pub fn enable_vt_sequences() -> std::io::Result<()> {
            Ok(()) // 非 Windows 系统无需特殊处理
        }

        pub fn get_terminal_path() -> Option<PathBuf> {
            None
        }

        pub fn get_windows_version() -> Option<String> {
            None
        }

        pub fn normalize_path(path: &str) -> String {
            path.to_string() // Unix 路径无需转换
        }

        pub fn is_windows_terminal() -> bool {
            false
        }

        pub fn is_wsl2() -> bool {
            false
        }
    }

    impl Default for ConPtyEnhanced {
        fn default() -> Self {
            Self::new()
        }
    }
}

/// 跨平台支持检测
pub struct PlatformCapabilities {
    pub is_windows: bool,
    pub is_wsl: bool,
    pub is_terminal_app: bool,
}

impl PlatformCapabilities {
    pub fn detect() -> Self {
        let is_windows = cfg!(target_os = "windows");
        let is_wsl = windows_support::ConPtyEnhanced::is_wsl2();
        let is_terminal_app = windows_support::ConPtyEnhanced::is_windows_terminal();

        PlatformCapabilities {
            is_windows,
            is_wsl,
            is_terminal_app,
        }
    }

    pub fn get_shell_command(&self) -> &'static str {
        if self.is_windows && !self.is_wsl {
            "powershell.exe" // Windows 原生
        } else if self.is_wsl {
            "/bin/bash" // WSL2 使用 bash
        } else {
            "/bin/sh" // Unix/Linux
        }
    }

    pub fn get_shell_args(&self) -> Vec<&'static str> {
        if self.is_windows && !self.is_wsl {
            vec!["-NoLogo", "-ExecutionPolicy", "RemoteSigned"]
        } else {
            vec!["-i", "-l"]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path_windows() {
        #[cfg(target_os = "windows")]
        {
            use crate::windows_support::ConPtyEnhanced;
            let path = ConPtyEnhanced::normalize_path("C:/Users/test/file.txt");
            assert_eq!(path, "C:\\Users\\test\\file.txt");
        }
    }

    #[test]
    fn test_platform_detection() {
        let caps = PlatformCapabilities::detect();
        assert!(caps.get_shell_command().len() > 0);
    }
}
