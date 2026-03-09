use k256::ecdsa::{SigningKey, Signature, signature::Signer};
use sha3::{Digest, Keccak256};
use std::str::FromStr;

pub struct HyperliquidSigner {
    key: SigningKey,
    wallet_address: String,
}

impl HyperliquidSigner {
    pub fn new(private_key_hex: &str) -> Self {
        let clean_hex = private_key_hex.trim_start_matches("0x");
        let key_bytes = hex::decode(clean_hex).expect("Hibás privát kulcs formátum");
        let key = SigningKey::from_slice(&key_bytes).expect("Érvénytelen ECDSA kulcs");
        
        // Cím generálása a publikus kulcsból (Keccak256 a uncompressed pubkey utolsó 64 bájtjára)
        let pubkey = key.verifying_key();
        let pubkey_bytes = pubkey.to_encoded_point(false);
        let mut hasher = Keccak256::new();
        hasher.update(&pubkey_bytes.as_bytes()[1..]);
        let hash = hasher.finalize();
        
        let mut address = String::with_capacity(42);
        address.push_str("0x");
        address.push_str(&hex::encode(&hash[12..]));

        Self {
            key,
            wallet_address: address,
        }
    }

    pub fn get_address(&self) -> &str {
        &self.wallet_address
    }

    /// Implementálja a Hyperliquid egyedi EIP-712 aláírását a cselekményekhez
    /// TODO: Ezt bővíteni kell a tényleges Order / Cancel sémával
    pub fn sign_l1_action(&self, action_hash: &[u8; 32], nonce: u64) -> Signature {
        // Pseudo kód az L1 aláírás gyorsításához
        let mut hasher = Keccak256::new();
        hasher.update(b"\x19\x01"); // EIP-712 prefix
        // domain separator, struct hash stb...
        hasher.update(action_hash);

        let digest = hasher.finalize();
        let (sig, _rec_id) = self.key.sign_prehash_recoverable(&digest).unwrap();
        
        sig
    }
}
