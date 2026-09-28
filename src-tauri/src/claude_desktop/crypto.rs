use super::Error;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::*,
    },
};
use zeroize::{Zeroize, Zeroizing};

pub(super) fn unprotect(input: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if input.is_empty() || input.len() > 4096 {
        return Err(Error::Unavailable);
    }
    let blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|_| Error::Unavailable)?;
        let result = if output.cbData == 32 && !output.pbData.is_null() {
            Ok(Zeroizing::new(
                std::slice::from_raw_parts(output.pbData, 32).to_vec(),
            ))
        } else {
            Err(Error::Unavailable)
        };
        if !output.pbData.is_null() {
            std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).zeroize();
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        }
        result
    }
}

struct Algorithm(BCRYPT_ALG_HANDLE);
impl Drop for Algorithm {
    fn drop(&mut self) {
        let _ = unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
    }
}
struct Key(BCRYPT_KEY_HANDLE);
impl Drop for Key {
    fn drop(&mut self) {
        let _ = unsafe { BCryptDestroyKey(self.0) };
    }
}

pub(super) fn decrypt(key: &[u8], encrypted: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if key.len() != 32
        || encrypted.len() < 31
        || encrypted.len() > 1024 * 1024
        || !encrypted.starts_with(b"v10")
    {
        return Err(Error::Unavailable);
    }
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_AES_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()
        .map_err(|_| Error::Unavailable)?;
        let algorithm = Algorithm(algorithm);
        let mode: Vec<u8> = "ChainingModeGCM\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        BCryptSetProperty(
            BCRYPT_HANDLE(algorithm.0 .0),
            BCRYPT_CHAINING_MODE,
            &mode,
            0,
        )
        .ok()
        .map_err(|_| Error::Unavailable)?;
        let mut handle = BCRYPT_KEY_HANDLE::default();
        // Windows 7+ owns the key object when pbKeyObject is null.
        BCryptGenerateSymmetricKey(algorithm.0, &mut handle, None, key, 0)
            .ok()
            .map_err(|_| Error::Unavailable)?;
        let handle = Key(handle);
        let mut nonce = encrypted[3..15].to_vec();
        let mut tag = encrypted[encrypted.len() - 16..].to_vec();
        let mut auth = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
            cbSize: std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
            dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
            pbNonce: nonce.as_mut_ptr(),
            cbNonce: 12,
            pbTag: tag.as_mut_ptr(),
            cbTag: 16,
            ..Default::default()
        };
        let ciphertext = &encrypted[15..encrypted.len() - 16];
        let mut output = Zeroizing::new(vec![0u8; ciphertext.len()]);
        let mut written = 0;
        BCryptDecrypt(
            handle.0,
            Some(ciphertext),
            Some((&mut auth as *mut BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO).cast()),
            None,
            Some(&mut output),
            &mut written,
            BCRYPT_FLAGS(0),
        )
        .ok()
        .map_err(|_| Error::Unavailable)?;
        if written as usize != output.len() {
            return Err(Error::Unavailable);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aes_gcm_known_vector_rejects_tampering_and_unknown_versions() {
        // NIST AES-256 GCM, zero key/nonce and one zero plaintext block.
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&[0; 12]);
        blob.extend_from_slice(&[
            0xce, 0xa7, 0x40, 0x3d, 0x4d, 0x60, 0x6b, 0x6e, 0x07, 0x4e, 0xc5, 0xd3, 0xba, 0xf3,
            0x9d, 0x18,
        ]);
        blob.extend_from_slice(&[
            0xd0, 0xd1, 0xc8, 0xa7, 0x99, 0x99, 0x6b, 0xf0, 0x26, 0x5b, 0x98, 0xb5, 0xd4, 0x8a,
            0xb9, 0x19,
        ]);
        assert_eq!(decrypt(&[0; 32], &blob).unwrap().as_slice(), &[0; 16]);
        blob[20] ^= 1;
        assert!(decrypt(&[0; 32], &blob).is_err());
        blob[20] ^= 1;
        let last = blob.len() - 1;
        blob[last] ^= 1;
        assert!(decrypt(&[0; 32], &blob).is_err());
        blob[1] = b'2';
        assert!(decrypt(&[0; 32], &blob).is_err());
        assert!(decrypt(&[0; 31], &blob).is_err());
        assert!(decrypt(&[0; 32], b"v10").is_err());
    }
    #[test]
    fn dpapi_round_trip_and_bad_input() {
        let mut key = [7u8; 32];
        let input = CRYPT_INTEGER_BLOB {
            cbData: 32,
            pbData: key.as_mut_ptr(),
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .unwrap();
            let protected = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
            assert_eq!(unprotect(&protected).unwrap().as_slice(), &key);
        }
        assert!(unprotect(b"invalid fixture").is_err());
    }
}
