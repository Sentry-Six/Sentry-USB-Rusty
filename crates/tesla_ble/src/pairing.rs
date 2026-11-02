//! Native add-key-to-whitelist (BLE pairing) request.
//!
//! Mirrors tesla-control's `add-key-request` (`SendAddKeyRequestWithRole`
//! in vehicle-command): builds the unauthenticated VCSEC
//! `WhitelistOperation::AddKeyToWhitelistAndAddPermissions` and wraps it
//! in a `ToVCSECMessage{ SignedMessage{ signatureType: PRESENT_KEY } }`.
//! The car receives it, prompts for an NFC-card tap on the center
//! console, and (on tap) enrols our public key.
//!
//! This is fire-and-forget: there is no signed session and the car may
//! not send a response, so callers send it via [`crate::gatt::Connection::write_frame`]
//! and confirm enrolment separately with a `session-info` probe. It
//! replaces the last runtime dependency on the external `tesla-control`
//! binary.

use prost::Message;

use crate::proto::keys::Role;
use crate::proto::vcsec::{
    KeyFormFactor, KeyMetadata, PermissionChange, PublicKey, SignatureType, SignedMessage,
    ToVcsecMessage, UnsignedMessage, WhitelistOperation, unsigned_message, whitelist_operation,
};

/// Roles offered by SentryUSB's pairing UI. Other Tesla roles are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRole {
    Owner,
    Driver,
}

impl PairingRole {
    pub fn pair_verb(self) -> &'static str {
        match self {
            Self::Owner => "pair",
            Self::Driver => "pair-driver",
        }
    }

    pub fn from_pair_verb(verb: &str) -> Option<Self> {
        match verb {
            "pair" => Some(Self::Owner),
            "pair-driver" => Some(Self::Driver),
            _ => None,
        }
    }
}

/// Build the serialized `ToVCSECMessage` bytes for an
/// add-key-to-whitelist request enrolling `pub_uncompressed` (the 65-byte
/// uncompressed P-256 point `0x04 || X || Y`) with the selected role and a
/// CLOUD_KEY form factor. Owner preserves the original pairing behavior;
/// Driver is experimental and may restrict some vehicle functions.
///
/// The GATT transport applies the 2-byte length framing; this returns the
/// raw protobuf payload.
pub fn build_add_key_request(pub_uncompressed: &[u8], role: PairingRole) -> Vec<u8> {
    let whitelist_op = WhitelistOperation {
        sub_message: Some(
            whitelist_operation::SubMessage::AddKeyToWhitelistAndAddPermissions(PermissionChange {
                key: Some(PublicKey {
                    public_key_raw: pub_uncompressed.to_vec(),
                }),
                seconds_to_be_active: 0,
                key_role: match role {
                    PairingRole::Owner => Role::Owner as i32,
                    PairingRole::Driver => Role::Driver as i32,
                },
            }),
        ),
        metadata_for_key: Some(KeyMetadata {
            key_form_factor: KeyFormFactor::CloudKey as i32,
        }),
    };
    let unsigned = UnsignedMessage {
        sub_message: Some(unsigned_message::SubMessage::WhitelistOperation(
            whitelist_op,
        )),
    };
    let envelope = ToVcsecMessage {
        signed_message: Some(SignedMessage {
            protobuf_message_as_bytes: unsigned.encode_to_vec(),
            signature_type: SignatureType::PresentKey as i32,
        }),
    };
    envelope.encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_pairing_payload(role: PairingRole, expected_role: Role) {
        // 65-byte uncompressed P-256 point shape (0x04 || 64 bytes).
        let mut pubkey = vec![0x04u8];
        pubkey.extend(std::iter::repeat_n(0xABu8, 64));

        let bytes = build_add_key_request(&pubkey, role);
        // Decodes as a ToVCSECMessage with a PRESENT_KEY signed message.
        let env = ToVcsecMessage::decode(bytes.as_slice()).expect("decode ToVCSECMessage");
        let signed = env.signed_message.expect("signed_message present");
        assert_eq!(signed.signature_type, SignatureType::PresentKey as i32);

        // The inner UnsignedMessage carries our key and selected role.
        let unsigned =
            UnsignedMessage::decode(signed.protobuf_message_as_bytes.as_slice()).expect("inner");
        let Some(unsigned_message::SubMessage::WhitelistOperation(op)) = unsigned.sub_message else {
            panic!("expected WhitelistOperation");
        };
        let Some(whitelist_operation::SubMessage::AddKeyToWhitelistAndAddPermissions(pc)) =
            op.sub_message
        else {
            panic!("expected AddKeyToWhitelistAndAddPermissions");
        };
        assert_eq!(pc.key.expect("key").public_key_raw, pubkey);
        assert_eq!(pc.key_role, expected_role as i32);
        assert_eq!(
            op.metadata_for_key.expect("metadata").key_form_factor,
            KeyFormFactor::CloudKey as i32
        );
    }

    #[test]
    fn owner_pairing_preserves_original_payload() {
        assert_pairing_payload(PairingRole::Owner, Role::Owner);
    }

    #[test]
    fn driver_pairing_encodes_driver_with_same_form_factor() {
        assert_pairing_payload(PairingRole::Driver, Role::Driver);
    }

    #[test]
    fn cli_and_ipc_pairing_roles_are_explicit() {
        assert_eq!(PairingRole::from_pair_verb("pair"), Some(PairingRole::Owner));
        assert_eq!(PairingRole::from_pair_verb("pair-driver"), Some(PairingRole::Driver));
        for role in [PairingRole::Owner, PairingRole::Driver] {
            assert_eq!(PairingRole::from_pair_verb(role.pair_verb()), Some(role));
        }
        assert_eq!(PairingRole::from_pair_verb("pair-admin"), None);
    }
}
