//! API keys the user pastes in Settings (OpenRouter, Z.ai), kept in Windows
//! Credential Manager ("Quota:<provider>"), encrypted for this Windows user.
//! Never written to Quota's own files, never sent to the UI once saved.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(id: &str) -> Vec<u16> {
        wide(&format!("Quota:{id}"))
    }

    pub fn get(id: &str) -> Option<String> {
        let name = target(id);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: CredReadW fills `cred` on success; it's freed below with CredFree.
        unsafe {
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                return None;
            }
            let c = &*cred;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec();
            CredFree(cred as *const _);
            String::from_utf8(bytes).ok().filter(|s| !s.is_empty())
        }
    }

    pub fn set(id: &str, secret: &str) -> Result<(), String> {
        let mut name = target(id);
        let mut user = wide("Quota");
        let mut blob = secret.as_bytes().to_vec();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: unsafe { std::mem::zeroed() },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        // SAFETY: every pointer in `cred` outlives the call.
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err("Windows couldn't save the key".into());
        }
        Ok(())
    }

    pub fn delete(id: &str) -> Result<(), String> {
        let name = target(id);
        // SAFETY: plain call with a NUL-terminated string.
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 && unsafe { GetLastError() } != ERROR_NOT_FOUND {
            return Err("Windows couldn't remove the key".into());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn get(_: &str) -> Option<String> {
        None
    }
    pub fn set(_: &str, _: &str) -> Result<(), String> {
        Err("Saving keys needs Windows".into())
    }
    pub fn delete(_: &str) -> Result<(), String> {
        Ok(())
    }
}

pub use imp::{delete, get, set};

/// Where a provider's API key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum KeySource {
    Saved,
    Env,
    None,
}

/// The key to use: one saved in Quota wins over the environment.
pub fn api_key(id: &str, env_vars: &[&str]) -> (Option<String>, KeySource) {
    if let Some(k) = get(id).map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        return (Some(k), KeySource::Saved);
    }
    for var in env_vars {
        if let Some(k) = std::env::var(var).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
            return (Some(k), KeySource::Env);
        }
    }
    (None, KeySource::None)
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn round_trip() {
        let id = format!("test-{}", std::process::id());
        super::set(&id, "sk-test-123").unwrap();
        assert_eq!(super::get(&id).as_deref(), Some("sk-test-123"));
        super::delete(&id).unwrap();
        assert_eq!(super::get(&id), None);
        super::delete(&id).unwrap();
    }
}
