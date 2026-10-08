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
    /// Classical Public Key Hash (P2PKH - Ed25519)
    PubKeyHash = 0x00,
    /// Reserved for a post-quantum key hash (no signature scheme implemented yet)
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

    /// Network identifier committed to by transaction signatures.
    pub fn id(&self) -> u8 {
        match self {
            Network::Mainnet => 0,
            Network::Testnet => 1,
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

    /// Encodes to human-readable string format (e.g. `imn:qz6r7...` or `imntest:qz6r7...`).
    /// This is Bech32 with a `:` in place of the `1` separator.
    pub fn encode(&self) -> Result<String, AddressError> {
        let hrp = Hrp::parse(self.network.hrp_str()).map_err(|e| AddressError::Encode(e.to_string()))?;
        
        let mut data = Vec::with_capacity(33);
        data.push(self.address_type as u8);
        data.extend_from_slice(self.hash.as_bytes());

        let encoded = bech32::encode::<Bech32>(hrp, &data)
            .map_err(|e| AddressError::Encode(e.to_string()))?;
        
        // Replace the Bech32 `1` separator with a colon: "imn1qz..." -> "imn:qz..."
        let prefix = self.network.hrp_str();
        match encoded.strip_prefix(prefix).and_then(|rest| rest.strip_prefix('1')) {
            Some(payload) => Ok(format!("{}:{}", prefix, payload)),
            None => Err(AddressError::Encode("unexpected Bech32 layout".to_string())),
        }
    }

    /// Decodes a human-readable Bech32 string back into an Address struct.
    pub fn decode(s: &str) -> Result<Self, AddressError> {
        // Restore the Bech32 separator (e.g. "imn:qz..." -> "imn1qz...")
        let (prefix, payload) = s
            .split_once(':')
            .ok_or_else(|| AddressError::Decode("missing ':' after network prefix".to_string()))?;
        let normalized = format!("{}1{}", prefix, payload);

        let (hrp, data) = bech32::decode(&normalized)
            .map_err(|e| AddressError::Decode(e.to_string()))?;

        // Upper case is allowed (QR codes use it); mixed case is refused by the Bech32 decoder
        let network = match hrp.as_str().to_ascii_lowercase().as_str() {
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

        // One spelling per address: the decoder also understands the Bech32m checksum, which
        // this network does not use
        let address = Self { network, address_type, hash };
        if !address.encode()?.eq_ignore_ascii_case(s) {
            return Err(AddressError::Decode("not the canonical spelling of this address".to_string()));
        }
        Ok(address)
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
    #[test]
    fn only_the_canonical_spelling_decodes() {
        use super::*;
        let address = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &[0x33u8; 32]);
        let text = address.to_string();
        assert_eq!(Address::decode(&text).unwrap(), address);
        assert_eq!(Address::decode(&text.to_uppercase()).unwrap(), address);

        // The same data with a Bech32m checksum is a different string for the same address
        let mut data = vec![address.address_type as u8];
        data.extend_from_slice(address.hash.as_bytes());
        let other = bech32::encode::<bech32::Bech32m>(Hrp::parse("imntest").unwrap(), &data).unwrap();
        let other = other.replacen("imntest1", "imntest:", 1);
        assert_ne!(other, text);
        assert!(Address::decode(&other).is_err());
    }

    use super::*;

    #[test]
    fn test_address_encode_decode_roundtrip() {
        let dummy_pubkey = [0x55u8; 32];
        let addr = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &dummy_pubkey);
        let encoded = addr.encode().expect("encoding must succeed");

        assert!(encoded.starts_with("imntest:q"), "{}", encoded);
        println!("Generated Testnet Address: {}", encoded);

        let decoded = Address::decode(&encoded).expect("decoding must succeed");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_mainnet_address() {
        let dummy_pubkey = [0xabu8; 32];
        let addr = Address::from_public_key(Network::Mainnet, AddressType::PostQuantumHash, &dummy_pubkey);
        let encoded = addr.encode().expect("encoding must succeed");

        assert!(encoded.starts_with("imn:q"), "{}", encoded);
        println!("Generated Mainnet Post-Quantum Address: {}", encoded);

        let decoded = Address::decode(&encoded).expect("decoding must succeed");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_decode_rejects_malformed_addresses() {
        let addr = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &[0x55u8; 32]);
        let encoded = addr.encode().unwrap();

        // Plain Bech32 form without the colon is not an address
        assert!(Address::decode(&encoded.replace(':', "1")).is_err());
        // A single changed character breaks the checksum
        let mut corrupted = encoded.clone().into_bytes();
        let last = corrupted.len() - 1;
        corrupted[last] = if corrupted[last] == b'q' { b'p' } else { b'q' };
        assert!(Address::decode(std::str::from_utf8(&corrupted).unwrap()).is_err());
        // Wrong network prefix
        assert!(Address::decode(&encoded.replace("imntest:", "btc:")).is_err());
    }
}
