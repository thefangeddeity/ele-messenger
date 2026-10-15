use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use crate::proto::NodeId;

pub const NOISE_PARAMS: &str = "Noise_XXpsk0_25519_ChaChaPoly_SHA256";

pub struct Identity {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
    pub id: NodeId,
}

pub fn node_id(public: &[u8]) -> NodeId {
    let h = Sha256::digest(public);
    let mut id = [0u8; 8];
    id.copy_from_slice(&h[..8]);
    id
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let kp = snow::Builder::new(NOISE_PARAMS.parse()?).generate_keypair()?;
        Ok(Self { id: node_id(&kp.public), private: kp.private.to_vec(), public: kp.public.to_vec() })
    }

    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let path = dir.join("identity.key");
        if path.exists() {
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            if bytes.len() != 64 {
                bail!("{} is corrupt", path.display());
            }
            let (private, public) = bytes.split_at(32);
            return Ok(Self { id: node_id(public), private: private.to_vec(), public: public.to_vec() });
        }
        let me = Self::generate()?;
        write_private(&path, &[me.private.as_slice(), me.public.as_slice()].concat())?;
        Ok(me)
    }
}

/// Mesh credential: everyone who knows the passphrase derives the same PSK.
pub fn derive_psk(passphrase: &str) -> Result<[u8; 32]> {
    if passphrase.trim().len() < 8 {
        bail!("mesh passphrase must be at least 8 characters");
    }
    let mut out = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), b"elemesh-v1-mesh-psk", &mut out)
        .map_err(|e| anyhow::anyhow!("deriving mesh key: {e}"))?;
    Ok(out)
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
