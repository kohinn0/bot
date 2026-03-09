use ethers::core::types::{Signature, H256};
use ethers::signers::{LocalWallet, Signer, WalletError};
use ethers::core::types::transaction::eip712::{Eip712, EIP712Domain};
use std::str::FromStr;

#[derive(Clone)]
pub struct HyperliquidSigner {
    wallet: LocalWallet,
    address: String,
    domain: EIP712Domain,
}

impl HyperliquidSigner {
    pub fn new(private_key_hex: &str) -> Self {
        let clean_hex = private_key_hex.trim_start_matches("0x");
        let wallet = LocalWallet::from_str(clean_hex)
            .expect("Hibás privát kulcs formátum vagy érvénytelen ECDSA kulcs");
            
        let address = format!("{:?}", wallet.address());

        // A Hyperliquid EIP-712 domain definíciója (Exchange specifikus)
        let domain = EIP712Domain {
            name: Some("HyperliquidSignTransaction".to_string()),
            version: Some("1".to_string()),
            chain_id: Some(421614.into()), // Arbitrum Goerli (minden HL EIP-712 műveletnél ezt használják)
            verifying_contract: Some(ethers::core::types::Address::from_str("0x0000000000000000000000000000000000000000").unwrap()),
            salt: None,
        };

        Self {
            wallet,
            address,
            domain,
        }
    }

    pub fn get_address(&self) -> &str {
        &self.address
    }

    /// Hyperliquid EIP-712 aláírás
    /// A payload aláírásához a JSON actiont MsgPack formátumra kell szerializálni,
    /// majd ezt le-keccak256-ozni. A végeredményt az EIP-712 domain structtal kódoljuk.
    pub async fn sign_l1_action(&self, action: serde_json::Value, nonce: u64, is_mainnet: bool) -> Result<Signature, WalletError> {
        // 1. Convert JSON Action to MessagePack bytes
        let msgpack_bytes = rmp_serde::to_vec_named(&action)
            .expect("Failed to serialize Action to MessagePack");

        // 2. Hash the MsgPack bytes (action_hash)
        let action_hash = ethers::utils::keccak256(&msgpack_bytes);

        // 3. Define the Chain ID
        let chain_id: u64 = if is_mainnet { 42161 } else { 421614 };

        // 4. Construct EIP-712 typed data manually
        // HL EIP712 standard is a bit unique. We hash the domain separator and the struct hash.
        
        let mut domain_hasher = ethers::utils::Keccak256::new();
        // type_hash for EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)
        // Keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
        let domain_type_hash = hex::decode("8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f").unwrap();
        domain_hasher.update(&domain_type_hash);
        
        let name_hash = ethers::utils::keccak256(b"HyperliquidSignTransaction");
        domain_hasher.update(&name_hash);
        
        let version_hash = ethers::utils::keccak256(b"1");
        domain_hasher.update(&version_hash);
        
        let mut chain_id_bytes = [0u8; 32];
        chain_id_bytes[24..32].copy_from_slice(&chain_id.to_be_bytes());
        domain_hasher.update(&chain_id_bytes);
        
        let verifying_contract = [0u8; 32]; // Zero address padded to 32 bytes
        domain_hasher.update(&verifying_contract);
        
        let domain_separator = domain_hasher.finalize();

        // Object struct hash
        let mut struct_hasher = ethers::utils::Keccak256::new();
        // type_hash for HyperliquidTransaction:Agent(address source,address connectionId) or whatever the type is.
        // For general L1 actions, Hyperliquid typically type-hashes "HyperliquidTransaction(bytes32 action,uint64 nonce)"
        // Keccak256("HyperliquidTransaction(bytes32 action,uint64 nonce)")
        let tx_type_hash = hex::decode("c4bc9fca5eb8b070def0d15ebc2c8f0e5c94294028ddc80ee9e96f13b6329fc6").unwrap();
        struct_hasher.update(&tx_type_hash);
        struct_hasher.update(&action_hash);
        
        let mut nonce_bytes = [0u8; 32];
        nonce_bytes[24..32].copy_from_slice(&nonce.to_be_bytes());
        struct_hasher.update(&nonce_bytes);
        
        let struct_hash = struct_hasher.finalize();

        // 5. Build final EIP-712 Digest
        let mut digest_hasher = ethers::utils::Keccak256::new();
        digest_hasher.update(&[0x19, 0x01]);
        digest_hasher.update(&domain_separator);
        digest_hasher.update(&struct_hash);
        let digest = digest_hasher.finalize();

        // 6. Sign the final digest
        self.wallet.sign_hash(H256::from(digest))
    }
}
