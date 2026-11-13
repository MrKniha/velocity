/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Content process lockdown. Web pages run in content processes (Servo multiprocess mode); Servo
//! 0.8.0 has no sandbox on Windows (`create_sandbox` panics there), so the shell restricts each
//! content process itself, first thing, before it connects to the browser and receives any page.
//!
//! - A job object: the process cannot start other programs, cannot touch the clipboard, other
//!   windows, global atoms, display or system settings, and cannot log off or shut down Windows.
//!   The job handle is closed afterwards, so code that takes over the process cannot lift it.
//! - Mitigation policies: no DLLs from remote shares or low-integrity files, no legacy extension
//!   DLL injection (AppInit, hooks, IMEs from outside), and invalid handle use crashes the process.
//! - Low integrity: the process can no longer write the user's files, registry or Medium
//!   processes and windows (it can still read what the user can read). The patched ipc-channel
//!   (vendor/README.md) makes IPC work from Low: the browser pulls the handles a Low process sends.
//!   The process object is relabelled Low first, so content processes can still pass each other
//!   handles.
//!
//! shortcut: the token keeps the user's groups and privileges (Chromium also restricts those and
//! uses an alternate desktop), and the lockdown happens in the process after it starts rather than
//! in the token it starts with. Upgrade with a restricted token at CreateProcess in the
//! constellation's `spawn_multiprocess`.

#[cfg(windows)]
pub fn lock_down() -> Result<(), String> {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::*;

    let error = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(error("CreateJobObject"));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP |
                JOB_OBJECT_UILIMIT_DISPLAYSETTINGS |
                JOB_OBJECT_UILIMIT_EXITWINDOWS |
                JOB_OBJECT_UILIMIT_GLOBALATOMS |
                JOB_OBJECT_UILIMIT_HANDLES |
                JOB_OBJECT_UILIMIT_READCLIPBOARD |
                JOB_OBJECT_UILIMIT_WRITECLIPBOARD |
                JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        };
        let applied = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0 &&
            SetInformationJobObject(
                job,
                JobObjectBasicUIRestrictions,
                &ui as *const _ as *const c_void,
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            ) != 0 &&
            AssignProcessToJobObject(job, GetCurrentProcess()) != 0;
        let result = if applied { Ok(()) } else { Err(error("job object")) };
        CloseHandle(job);
        result?;

        // Each policy structure is one DWORD of flag bits.
        let policies: [(PROCESS_MITIGATION_POLICY, u32, &str); 3] = [
            (ProcessExtensionPointDisablePolicy, 0b1, "extension point policy"), // DisableExtensionPoints
            (ProcessImageLoadPolicy, 0b11, "image load policy"), // NoRemoteImages, NoLowMandatoryLabelImages
            (ProcessStrictHandleCheckPolicy, 0b11, "strict handle policy"), // Raise..., ...PermanentlyEnabled
        ];
        for (policy, flags, what) in policies {
            if SetProcessMitigationPolicy(policy, &flags as *const u32 as *const c_void, size_of::<u32>()) == 0 {
                return Err(error(what));
            }
        }

        lower_to_low_integrity().map_err(|what| error(what))?;
    }
    Ok(())
}

/// Relabel this process object Low, then drop its token to Low integrity. Errors name the step.
#[cfg(windows)]
fn lower_to_low_integrity() -> Result<(), &'static str> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Threading::*;

    unsafe {
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();

        // Process object label: Low, no-write-up and no-read-up, as a Low process would get.
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide("S:(ML;;NWNR;;;LW)").as_ptr(), SDDL_REVISION_1, &mut descriptor, null_mut()) == 0 {
            return Err("label descriptor");
        }
        let (mut present, mut defaulted, mut sacl) = (0, 0, null_mut());
        let relabelled = GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted) != 0 &&
            SetSecurityInfo(GetCurrentProcess(), SE_KERNEL_OBJECT, LABEL_SECURITY_INFORMATION, null_mut(), null_mut(), null(), sacl) == 0;
        LocalFree(descriptor as _);
        if !relabelled {
            return Err("relabel process");
        }

        let mut sid: PSID = null_mut();
        if ConvertStringSidToSidW(wide("S-1-16-4096").as_ptr(), &mut sid) == 0 {
            return Err("Low integrity SID");
        }
        let mut token: HANDLE = null_mut();
        let lowered = OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_DEFAULT | TOKEN_QUERY, &mut token) != 0 && {
            let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: 0x20 /* SE_GROUP_INTEGRITY */ } };
            let ok = SetTokenInformation(
                token,
                TokenIntegrityLevel,
                &label as *const _ as *const c_void,
                size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid),
            ) != 0;
            CloseHandle(token);
            ok
        };
        LocalFree(sid as _);
        if lowered { Ok(()) } else { Err("lower token") }
    }
}

#[cfg(not(windows))]
pub fn lock_down() -> Result<(), String> {
    Err("no content process lockdown on this platform".into())
}

#[cfg(all(test, windows))]
mod tests {
    use std::process::Command;

    /// Runs in a child copy of this test binary: lock down, then try what a content process must not.
    #[test]
    fn locked_down_child() {
        if std::env::var_os("VELOCITY_SANDBOX_CHILD").is_none() {
            return;
        }
        super::lock_down().expect("lock down");
        assert!(Command::new("cmd").args(["/c", "exit"]).status().is_err(), "a locked-down process started a program");
        // Low integrity: the user's own folders (Medium) are read-only now.
        let file = std::env::temp_dir().join(format!("velocity-sandbox-{}.txt", std::process::id()));
        assert!(std::fs::write(&file, "x").is_err(), "a locked-down process wrote {}", file.display());
        assert!(std::fs::read_dir(std::env::temp_dir()).is_ok(), "reading still works");
        println!("SANDBOX-CHILD-OK");
    }

    #[test]
    fn content_process_cannot_start_programs_or_write_files() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sandbox::tests::locked_down_child", "--nocapture"])
            .env("VELOCITY_SANDBOX_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && stdout.contains("SANDBOX-CHILD-OK"), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    }
}
