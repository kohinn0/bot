use ethers::core::types::{Signature, H256};
use ethers::signers::{LocalWallet, Signer, WalletError};
use sha3::{Digest, Keccak256};
use std::str::FromStr;

#[derive(Clone)]
pub struct HyperliquidSigner {
    wallet: LocalWallet,
    address: String,
    domain_separator: [u8; 32],
    agent_type_hash: [u8; 32],
    source_a_hash: [u8; 32],
    source_b_hash: [u8; 32],
}

impl HyperliquidSigner {
    pub fn new(private_key_hex: &str) -> Self {
        let clean_hex = private_key_hex.trim_start_matches("0x");
        let wallet = LocalWallet::from_str(clean_hex)
            .expect("Hibás privát kulcs formátum vagy érvénytelen ECDSA kulcs");
            
        // EIP-55 checksum cím — így egyezik a Hyperliquid webes portfólióval; a `{:?}` formátum eltérhet.
        let address = format!("{}", wallet.address());

        // 1. Pre-calculate Domain Separator
        let mut domain_hasher = Keccak256::new();
        let domain_type_hash = hex::decode("8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f").unwrap();
        domain_hasher.update(&domain_type_hash);
        domain_hasher.update(&ethers::utils::keccak256(b"Exchange"));
        domain_hasher.update(&ethers::utils::keccak256(b"1"));
        
        let mut chain_id_bytes = [0u8; 32];
        let chain_id: u64 = 1337;
        chain_id_bytes[24..32].copy_from_slice(&chain_id.to_be_bytes());
        domain_hasher.update(&chain_id_bytes);
        domain_hasher.update(&[0u8; 32]); // verifyingContract
        
        let domain_separator: [u8; 32] = domain_hasher.finalize().into();

        // 2. Pre-calculate Agent hashes
        let agent_type_hash: [u8; 32] = ethers::utils::keccak256(b"Agent(string source,bytes32 connectionId)").into();
        let source_a_hash: [u8; 32] = ethers::utils::keccak256(b"a").into();
        let source_b_hash: [u8; 32] = ethers::utils::keccak256(b"b").into();

        Self {
            wallet,
            address,
            domain_separator,
            agent_type_hash,
            source_a_hash,
            source_b_hash,
        }
    }

    pub fn get_address(&self) -> &str {
        &self.address
    }

    pub async fn sign_l1_action<T: serde::Serialize>(&self, action: &T, nonce: u64, is_mainnet: bool) -> Result<Signature, WalletError> {
        // 1. serialize to MessagePack
        let mut data = rmp_serde::to_vec_named(action)
            .expect("Failed to serialize Action to MessagePack");
            
        // 2. Add Nonce and Vault flag
        data.extend_from_slice(&nonce.to_be_bytes());
        data.push(0x00);
        
        let connection_id = ethers::utils::keccak256(&data);

        // 3. Struct Hash (Agent)
        let mut struct_hasher = Keccak256::new();
        struct_hasher.update(&self.agent_type_hash);
        
        let source_hash = if is_mainnet { self.source_a_hash } else { self.source_b_hash };
        struct_hasher.update(&source_hash);
        struct_hasher.update(&connection_id);
        
        let struct_hash: [u8; 32] = struct_hasher.finalize().into();

        // 4. Build final EIP-712 Digest
        let mut digest_hasher = Keccak256::new();
        digest_hasher.update(&[0x19, 0x01]);
        digest_hasher.update(&self.domain_separator);
        digest_hasher.update(&struct_hash);
        
        let digest: [u8; 32] = digest_hasher.finalize().into();

        // 5. Sign
        self.wallet.sign_hash(H256::from(digest))
    }
}
