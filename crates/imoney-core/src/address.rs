use crate::hash::Hash;
use bech32::{Bech32, Hrp};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum AddressError {
    #[error("Invalid address HRP prefix: {0}")]
    InvalidHrp(String),
    #[error("Invalid payload length: expected 33 bytes, got {0}")]
    InvalidLength(usize),
    #[error("Bech32 decode error: {0}")]
    Decode(String),
    #[error("Bech32 encode error: {0}")]
    Encode(String),
}

/// Address type classifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AddressType {
    /// Classical Public Key Hash (P2PKH - Schnorr/Ed25519)
    PubKeyHash = 0x00,
    /// Post-Quantum Public Key Hash (P2PQ - Falcon-512)
    PostQuantumHash = 0x01,
    /// Multi-signature Script Hash (P2SH)
    ScriptHash = 0x02,
}

impl AddressType {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x00 => Some(AddressType::PubKeyHash),
            0x01 => Some(AddressType::PostQuantumHash),
            0x02 => Some(AddressType::ScriptHash),
            _ => None,
        }
    }
}

/// Network classifier for human-readable prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    pub fn hrp_str(&self) -> &'static str {
        match self {
            Network::Mainnet => "imn",
            Network::Testnet => "imntest",
        }
    }
}

/// Checksummed human-readable Internet Money address (Bech32).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub network: Network,
    pub address_type: AddressType,
    pub hash: Hash,
}

impl Address {
    pub fn new(network: Network, address_type: AddressType, hash: Hash) -> Self {
        Self { network, address_type, hash }
    }

    /// Derives an address from a raw public key using Blake3.
    pub fn from_public_key(network: Network, address_type: AddressType, pubkey_bytes: &[u8]) -> Self {
        let hash = Hash(*blake3::hash(pubkey_bytes).as_bytes());
        Self::new(network, address_type, hash)
    }

    /// Encodes to human-readable Bech32 string format (e.g. `imn:qz6r7...` or `imntest:qz6r7...`).
    pub fn encode(&self) -> Result<String, AddressError> {
        let hrp = Hrp::parse(self.network.hrp_str()).map_err(|e| AddressError::Encode(e.to_string()))?;
        
        let mut data = Vec::with_capacity(33);
        data.push(self.address_type as u8);
        data.extend_from_slice(self.hash.as_bytes());

        let encoded = bech32::encode::<Bech32>(hrp, &data)
            .map_err(|e| AddressError::Encode(e.to_string()))?;
        
        // Kaspa / Bitcoin style prefix with colon: e.g. "imn:qz..."
        let prefix = self.network.hrp_str();
        if let Some(payload) = encoded.strip_prefix(prefix) {
            Ok(format!("{}:{}", prefix, payload))
        } else {
            Ok(encoded)
        }
    }

    /// Decodes a human-readable Bech32 string back into an Address struct.
    pub fn decode(s: &str) -> Result<Self, AddressError> {
        // Strip optional colon delimiter if present (e.g. "imn:qz..." -> "imnqz...")
        let normalized = if let Some((prefix, rest)) = s.split_once(':') {
            format!("{}{}", prefix, rest)
        } else {
            s.to_string()
        };

        let (hrp, data) = bech32::decode(&normalized)
            .map_err(|e| AddressError::Decode(e.to_string()))?;

        let network = match hrp.as_str() {
            "imn" => Network::Mainnet,
            "imntest" => Network::Testnet,
            other => return Err(AddressError::InvalidHrp(other.to_string())),
        };

        if data.len() != 33 {
            return Err(AddressError::InvalidLength(data.len()));
        }

        let address_type = AddressType::from_u8(data[0])
            .ok_or_else(|| AddressError::Decode("Unknown address type byte".to_string()))?;

        let mut hash_bytes = [0u8; 32];
        hash_bytes.copy_from_slice(&data[1..33]);
        let hash = Hash(hash_bytes);

        Ok(Self { network, address_type, hash })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.encode() {
            Ok(s) => write!(f, "{}", s),
            Err(_) => write!(f, "<invalid address>"),
        }
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_encode_decode_roundtrip() {
        let dummy_pubkey = [0x55u8; 32];
        let addr = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &dummy_pubkey);
        let encoded = addr.encode().expect("encoding must succeed");

        assert!(encoded.starts_with("imntest:"));
        println!("Generated Testnet Address: {}", encoded);

        let decoded = Address::decode(&encoded).expect("decoding must succeed");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_mainnet_address() {
        let dummy_pubkey = [0xabu8; 32];
        let addr = Address::from_public_key(Network::Mainnet, AddressType::PostQuantumHash, &dummy_pubkey);
        let encoded = addr.encode().expect("encoding must succeed");

        assert!(encoded.starts_with("imn:"));
        println!("Generated Mainnet Post-Quantum Address: {}", encoded);

        let decoded = Address::decode(&encoded).expect("decoding must succeed");
        assert_eq!(addr, decoded);
    }
}
