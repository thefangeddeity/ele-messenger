//! Entry signatures with the node's own X25519 identity key (XEdDSA, as
//! Signal specifies it). A node id is the hash of that public key, so a
//! signature carries its own proof of authorship: anyone holding the entry,
//! the key and the signature can check them, whoever relayed them, with no
//! key exchange and no one to trust.

use curve25519_dalek::constants::ED25519_BASEPOINT_TABLE;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::montgomery::MontgomeryPoint;
use curve25519_dalek::scalar::Scalar;
use rand::RngExt;
use sha2::{Digest, Sha512};

use crate::identity::node_id;
use crate::proto::NodeId;

fn wide(parts: &[&[u8]]) -> Scalar {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    let mut out = [0u8; 64];
    out.copy_from_slice(&h.finalize());
    Scalar::from_bytes_mod_order_wide(&out)
}

fn clamp(mut k: [u8; 32]) -> [u8; 32] {
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    k
}

/// Signs `msg` with an X25519 private key.
pub fn sign(private: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let k = Scalar::from_bytes_mod_order(clamp(*private));
    let e = (&k * ED25519_BASEPOINT_TABLE).compress();
    // XEdDSA fixes the public key's sign bit at 0: negate the scalar when
    // its Edwards point has the other sign.
    let a = if e.as_bytes()[31] & 0x80 != 0 { -k } else { k };
    let mut pub_a = e.to_bytes();
    pub_a[31] &= 0x7f;
    let z: [u8; 64] = rand::rng().random();
    let mut prefix = [0xffu8; 32];
    prefix[0] = 0xfe;
    let r = wide(&[&prefix, a.as_bytes(), msg, &z]);
    let big_r = (&r * ED25519_BASEPOINT_TABLE).compress();
    let h = wide(&[big_r.as_bytes(), &pub_a, msg]);
    let s = r + h * a;
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(big_r.as_bytes());
    sig[32..].copy_from_slice(s.as_bytes());
    sig
}

/// Checks a signature against an X25519 public key.
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Some(a) = MontgomeryPoint(*public).to_edwards(0) else { return false };
    let Some(big_r) = CompressedEdwardsY(sig[..32].try_into().unwrap()).decompress() else { return false };
    let Some(s) = Option::<Scalar>::from(Scalar::from_canonical_bytes(sig[32..].try_into().unwrap())) else {
        return false;
    };
    let h = wide(&[&sig[..32], a.compress().as_bytes(), msg]);
    EdwardsPoint::vartime_double_scalar_mul_basepoint(&h, &-a, &s) == big_r
}

/// Whether `key` is the identity key of `author`.
pub fn key_of(author: &NodeId, key: &[u8; 32]) -> bool {
    node_id(key) == *author
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> ([u8; 32], [u8; 32]) {
        let private: [u8; 32] = rand::rng().random();
        let public = MontgomeryPoint::mul_base_clamped(private).to_bytes();
        (private, public)
    }

    #[test]
    fn signs_and_verifies() {
        for _ in 0..32 {
            let (k, p) = keypair();
            let sig = sign(&k, b"hello mesh");
            assert!(verify(&p, b"hello mesh", &sig));
            assert!(!verify(&p, b"hello mess", &sig));
            let mut bad = sig;
            bad[40] ^= 1;
            assert!(!verify(&p, b"hello mesh", &bad));
            let (_, other) = keypair();
            assert!(!verify(&other, b"hello mesh", &sig));
        }
    }

    #[test]
    fn key_binds_to_author() {
        let (_, p) = keypair();
        assert!(key_of(&node_id(&p), &p));
        let (_, q) = keypair();
        assert!(!key_of(&node_id(&p), &q));
    }
}
