use anyhow::Result;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    },
};
use zeroize::{Zeroize, Zeroizing};

const CAP: usize = 2 * 1024 * 1024;

struct Output(CRYPT_INTEGER_BLOB);

impl Drop for Output {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            unsafe {
                std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                let _ = LocalFree(Some(HLOCAL(self.0.pbData.cast())));
            }
        }
    }
}

fn unavailable() -> anyhow::Error {
    anyhow::anyhow!("Antigravity CLI binding encryption unavailable")
}

pub(super) fn protect(input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    transform(input, true)
}

pub(super) fn unprotect(input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    transform(input, false)
}

fn transform(input: &[u8], encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    if input.is_empty() || input.len() > CAP {
        return Err(unavailable());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = Output(CRYPT_INTEGER_BLOB::default());
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        }
        .map_err(|_| unavailable())?;
        if output.0.pbData.is_null() || output.0.cbData == 0 || output.0.cbData as usize > CAP {
            return Err(unavailable());
        }
        Ok(Zeroizing::new(
            std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize).to_vec(),
        ))
    }
}
