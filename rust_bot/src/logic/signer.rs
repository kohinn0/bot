use ethers::core::types::{Signature, H256};
use ethers::signers::{LocalWallet, Signer, WalletError};
use ethers::core::types::transaction::eip712::EIP712Domain;
use sha3::{Digest, Keccak256};
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

    pub async fn sign_l1_action(&self, action: serde_json::Value, nonce: u64, is_mainnet: bool) -> Result<Signature, WalletError> {
        // 1. A json Action MessagePack formátummá konvertálása
        let mut data = rmp_serde::to_vec_named(&action)
            .expect("Failed to serialize Action to MessagePack");
            
        // 2. HL Specifikus: Hozzáfűzzük a Nonce-t (8 bájt, big endian) és a Vault flag-et (0x00)
        data.extend_from_slice(&nonce.to_be_bytes());
        data.push(0x00); // vault_address is None
        
        let connection_id = ethers::utils::keccak256(&data);

        // 3. Domain Separator generálása az "Exchange" L1 endpointra
        let mut domain_hasher = Keccak256::new();
        // Keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
        let domain_type_hash = hex::decode("8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f").unwrap();
        domain_hasher.update(&domain_type_hash);
        domain_hasher.update(&ethers::utils::keccak256(b"Exchange"));
        domain_hasher.update(&ethers::utils::keccak256(b"1"));
        
        let mut chain_id_bytes = [0u8; 32];
        let chain_id: u64 = 1337; // Az L1 Order műveletek mindig 1337-es Chain ID-n futnak
        chain_id_bytes[24..32].copy_from_slice(&chain_id.to_be_bytes());
        domain_hasher.update(&chain_id_bytes);
        
        let verifying_contract = [0u8; 32];
        domain_hasher.update(&verifying_contract);
        
        let ds_array = domain_hasher.finalize();
        let domain_separator: [u8; 32] = ds_array.into();

        // 4. Struct Hash generálása a Phantom Agentre
        let mut struct_hasher = Keccak256::new();
        // Keccak256("Agent(string source,bytes32 connectionId)")
        let agent_type_hash = ethers::utils::keccak256(b"Agent(string source,bytes32 connectionId)");
        struct_hasher.update(&agent_type_hash);
        
        let source_str = if is_mainnet { "a" } else { "b" };
        let source_hash = ethers::utils::keccak256(source_str.as_bytes());
        struct_hasher.update(&source_hash);
        struct_hasher.update(&connection_id);
        
        let sh_array = struct_hasher.finalize();
        let struct_hash: [u8; 32] = sh_array.into();

        // 5. Build final EIP-712 Digest
        let mut digest_hasher = Keccak256::new();
        digest_hasher.update(&[0x19, 0x01]);
        digest_hasher.update(&domain_separator);
        digest_hasher.update(&struct_hash);
        
        let digest_array = digest_hasher.finalize();
        let digest: [u8; 32] = digest_array.into();

        // 6. Sign
        self.wallet.sign_hash(H256::from(digest))
    }
}
