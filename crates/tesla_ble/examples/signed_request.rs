//! Send a signed VCSEC InformationRequest and observe what the car
//! returns.
//!
//! What this proves:
//!   * Our metadata TLV layout matches what the car reconstructs on
//!     its end (otherwise: MESSAGEFAULT_ERROR_INVALID_SIGNATURE).
//!   * Our counter is fresh enough that the car accepts the request
//!     (otherwise: MESSAGEFAULT_ERROR_INVALID_TOKEN_OR_COUNTER).
//!   * Our AES-GCM tag is computed correctly (otherwise: same as
//!     INVALID_SIGNATURE — the tag IS the signature for GCM).
//!
//! What it doesn't prove yet:
//!   * Response decryption (we just dump whatever comes back; if it's
//!     a SignedMessage_status with operationStatus=OK + a fault code,
//!     the request side worked even though we can't decrypt the
//!     response payload yet — that comes next iteration).
//!
//! Usage:
//!   sudo ./signed_request <VIN> <path-to-key.pem> [domain] [state]
//!
//! Where [domain] is "vcsec" (default) or "infotainment", and
//! [state] (Infotainment only) selects which state query to send:
//! "climate", "charge", "drive", "location", "closures", or "tires".
//! Defaults to "climate".
//!
//! Diagnostic interpretation of the fault code:
//!   * fault=5  (INVALID_SIGNATURE) on VCSEC  → likely VCSEC wants HMAC
//!   * fault=5  on Infotainment              → real metadata/crypto bug
//!   * fault=9  (INVALID_COMMAND)            → signature passed, payload
//!     just isn't a valid command for that domain (that's actually a WIN
//!     for us — proves the signature works)
//!   * fault=6  (INVALID_TOKEN_OR_COUNTER)    → counter race/replay

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use prost::Message;
use sentryusb_tesla_ble::{
    auth, crypto,
    gatt::Connection,
    keys::KeyPair,
    proto::{
        signatures::signature_data,
        universal_message::{Domain, RoutableMessage, routable_message, destination},
        vcsec::{InformationRequest, UnsignedMessage, unsigned_message},
    },
    scan, session,
    state_query::{self, VehicleDataState},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,btleplug=warn".into()),
        )
        .with_target(false)
        .init();

    let vin = std::env::args()
        .nth(1)
        .context("usage: signed_request <VIN> <path-to-key.pem> [domain]")?;
    let key_path: PathBuf = std::env::args()
        .nth(2)
        .context("usage: signed_request <VIN> <path-to-key.pem> [domain]")?
        .into();
    let domain_arg = std::env::args().nth(3).unwrap_or_else(|| "vcsec".into());
    let domain = match domain_arg.as_str() {
        "vcsec" | "vehicle-security" => Domain::VehicleSecurity,
        "infotainment" | "info" => Domain::Infotainment,
        other => anyhow::bail!(
            "unknown domain '{other}', expected 'vcsec' or 'infotainment'"
        ),
    };
    if vin.len() != 17 {
        anyhow::bail!("VIN must be 17 chars, got {}", vin.len());
    }

    let keypair = KeyPair::load(&key_path)?;
    println!("Our pubkey: {} bytes", keypair.pub_uncompressed.len());
    println!("Domain:     {:?}", domain);

    // --- Connect + handshake ---
    let adapter = scan::first_adapter().await?;
    let target = scan::scan_for_vin(&adapter, &vin, Duration::from_secs(30)).await?;
    let mut conn = Connection::open(target.peripheral).await?;
    let session_info = session::request_session_info(&mut conn, &keypair, domain).await?;

    println!();
    println!("=== Session ===");
    println!("  domain:      {:?}", session_info.domain);
    println!("  counter:     {} (car's last seen)", session_info.parsed.counter);
    println!("  clock_time:  {}", session_info.parsed.clock_time);
    println!("  epoch:       {} bytes", session_info.parsed.epoch.len());

    let session_key =
        crypto::derive_session_key(&keypair.secret, &session_info.parsed.public_key)?;
    println!("  session key: {}", hex::encode(session_key.as_bytes()));

    // --- Build the inner payload ---
    // For VCSEC (vehicle-security domain) we send a VCSEC UnsignedMessage.
    // For Infotainment we send a car_server.Action — built as raw bytes
    // by state_query::build_get_state_request so we don't need the
    // whole car_server.proto in our build.
    let state_arg = std::env::args().nth(4);
    let inner = if domain == Domain::Infotainment {
        let state = state_arg
            .as_deref()
            .and_then(VehicleDataState::from_str)
            .unwrap_or(VehicleDataState::Climate);
        println!("State query:  {:?}", state);
        state_query::build_get_state_request(state)
    } else {
        // VCSEC: empty InformationRequest = GET_STATUS request.
        UnsignedMessage {
            sub_message: Some(unsigned_message::SubMessage::InformationRequest(
                InformationRequest {
                    information_request_type: 0,
                    key: None,
                },
            )),
        }
        .encode_to_vec()
    };

    println!();
    println!("=== Inner payload ===");
    println!("  bytes: {} ({})", inner.len(), hex::encode(&inner));

    // --- Sign + send ---
    // Counter must be STRICTLY GREATER than session_info.parsed.counter.
    let counter = session_info.parsed.counter + 1;
    // Match tesla-control's window — captured wire trace shows ~200s
    // ahead of the vehicle clock at send time. Our prior 5s window
    // was tight enough to fail on clock skew between hops; 30s is a
    // safe middle.
    let expires_at = session_info.parsed.clock_time + 30;
    // tesla-control sets FLAG_ENCRYPT_RESPONSE (bit 1, value 2) on
    // every signed state query. That flag is part of the metadata
    // the car uses to verify the signature, so the value must match
    // both in metadata (auth::sign) AND on the outer RoutableMessage
    // (build_signed_routable_message).
    let flags = 2;

    let parts = auth::sign(
        &session_key,
        &keypair.pub_uncompressed,
        &inner,
        domain,
        vin.as_bytes(),
        &session_info.parsed.epoch,
        expires_at,
        counter,
        flags,
    )?;

    println!();
    println!("=== Signed ===");
    println!("  counter:     {}", parts.counter);
    println!("  ciphertext:  {} bytes ({})", parts.ciphertext.len(), hex::encode(&parts.ciphertext));

    let envelope = auth::build_signed_routable_message(&parts, domain, flags);
    println!("  envelope:    {} bytes", envelope.len());

    // --- Send + observe the response ---
    // Accept-everything validator — examples are diagnostic tools
    // where we want to surface even malformed responses to the
    // operator rather than silently retry past them.
    let resp_bytes = conn
        .round_trip(&envelope, Duration::from_secs(10), |_| true)
        .await
        .context("signed round-trip")?;

    println!();
    println!("=== Response (raw {} bytes) ===", resp_bytes.len());
    println!("  hex: {}", hex::encode(&resp_bytes));

    // Try to decode the outer envelope. We can read the status fields
    // even without decrypting the payload, so a MessageStatus with a
    // non-zero fault code is super useful diagnostic info.
    match RoutableMessage::decode(resp_bytes.as_slice()) {
        Ok(rm) => {
            println!();
            println!("=== Decoded outer envelope ===");

            let fault = rm
                .signed_message_status
                .as_ref()
                .map(|s| s.signed_message_fault as u32)
                .unwrap_or(0);
            let resp_flags = rm.flags;
            if let Some(status) = rm.signed_message_status.as_ref() {
                println!("  operation_status:     {:?}", status.operation_status);
                println!("  signed_message_fault: {:?}", status.signed_message_fault);
            } else {
                println!("  signed_message_status: (default — operation_status=Ok, fault=None)");
            }
            println!("  flags: {}", resp_flags);

            // Figure out what domain the response came FROM (for the
            // response metadata DOMAIN tag).
            let from_domain = match rm.from_destination.as_ref()
                .and_then(|d| d.sub_destination.as_ref())
            {
                Some(destination::SubDestination::Domain(d)) =>
                    Domain::try_from(*d).unwrap_or(domain),
                _ => domain,
            };

            // Extract the response's AES_GCM_Response signature_data.
            let response_sig = rm.sub_sig_data.as_ref().and_then(|s| match s {
                routable_message::SubSigData::SignatureData(sd) => sd.sig_type.as_ref().and_then(|t| match t {
                    signature_data::SigType::AesGcmResponseData(r) => Some(r),
                    _ => None,
                }),
            });

            // Pull out the encrypted payload.
            let encrypted_payload = rm.payload.as_ref().and_then(|p| match p {
                routable_message::Payload::ProtobufMessageAsBytes(b) => Some(b),
                _ => None,
            });

            match (encrypted_payload, response_sig) {
                (Some(ct), Some(resp_sig)) if !ct.is_empty() => {
                    println!("  inner payload: {} bytes (encrypted)", ct.len());
                    println!("  response sig: nonce={} bytes, counter={}, tag={} bytes",
                        resp_sig.nonce.len(), resp_sig.counter, resp_sig.tag.len());

                    // The request_tag we sent — needed for REQUEST_HASH binding.
                    let req_tag = match parts.signature_data.sig_type.as_ref() {
                        Some(signature_data::SigType::AesGcmPersonalizedData(p)) => &p.tag,
                        _ => unreachable!("we signed with AES_GCM_PERSONALIZED"),
                    };

                    match auth::decrypt_response(
                        &session_key,
                        req_tag,
                        from_domain,
                        vin.as_bytes(),
                        resp_flags,
                        resp_sig.counter,
                        fault,
                        &resp_sig.nonce,
                        &resp_sig.tag,
                        ct,
                    ) {
                        Ok(plain) => {
                            println!();
                            println!("=== Decrypted response payload ({} bytes) ===", plain.len());
                            println!("hex: {}", hex::encode(&plain));
                        }
                        Err(e) => {
                            println!("  decrypt FAILED: {e}");
                        }
                    }
                }
                (Some(ct), _) if ct.is_empty() => {
                    println!("  inner payload: empty (car has no payload to return for this fault)");
                }
                _ => {
                    // SessionInfo refresh or some non-encrypted variant.
                    if let Some(payload) = rm.payload {
                        match payload {
                            routable_message::Payload::SessionInfo(b) => {
                                println!("  payload is a SessionInfo refresh ({} bytes)", b.len());
                                println!("    (car wants us to re-handshake)");
                            }
                            other => println!("  payload variant: {:?}", other),
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!();
            println!("Could not decode as RoutableMessage: {e}");
        }
    }

    conn.close().await;
    Ok(())
}
