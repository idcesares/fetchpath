//! Protection for the device identity key at rest.

use std::io;

/// Seals a secret so that only this user on this machine can read it back.
pub trait SecretProtector {
    fn protect(&self, plaintext: &[u8]) -> io::Result<Vec<u8>>;
    fn unprotect(&self, protected: &[u8]) -> io::Result<Vec<u8>>;
}

/// Windows DPAPI, scoped to the current user, with an entropy label distinct
/// from the browser inbox so neither blob can be unsealed as the other.
pub struct Dpapi;

const ENTROPY: &[u8] = b"fetchpath-lan-identity-v1";

impl SecretProtector for Dpapi {
    fn protect(&self, plaintext: &[u8]) -> io::Result<Vec<u8>> {
        dpapi::protect(plaintext, ENTROPY)
    }

    fn unprotect(&self, protected: &[u8]) -> io::Result<Vec<u8>> {
        dpapi::unprotect(protected, ENTROPY)
    }
}

#[cfg(windows)]
mod dpapi {
    use std::io;
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(bytes: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: bytes
                .len()
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "secret too large"))?,
            pbData: bytes.as_ptr().cast_mut(),
        })
    }

    /// Copies the system-allocated output, wipes it, and frees it exactly once.
    ///
    /// # Safety
    /// `output` must have been filled by a successful DPAPI call.
    unsafe fn take(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // An empty result may come back without a buffer, and a slice may not
        // be built from a null pointer even at length zero.
        if output.pbData.is_null() {
            return Vec::new();
        }
        let len = output.cbData as usize;
        let bytes = unsafe { std::slice::from_raw_parts(output.pbData, len) }.to_vec();
        unsafe {
            ptr::write_bytes(output.pbData, 0, len);
            LocalFree(output.pbData.cast());
        }
        bytes
    }

    pub fn protect(plaintext: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(plaintext)?;
        let entropy = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: every pointer refers to a live local for the whole call; the
        // input buffers are only read. Output is freed by `take`.
        let success = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { take(output) })
    }

    pub fn unprotect(protected: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(protected)?;
        let entropy = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`.
        let success = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { take(output) })
    }
}

#[cfg(not(windows))]
mod dpapi {
    use std::io;

    pub fn protect(_: &[u8], _: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "DPAPI requires Windows",
        ))
    }

    pub fn unprotect(_: &[u8], _: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "DPAPI requires Windows",
        ))
    }
}
