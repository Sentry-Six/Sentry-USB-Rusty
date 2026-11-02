//! Hand-built `car_server.Action` inner payloads for state queries
//! (climate, charge, drive, location, etc.).
//!
//! tesla-control sends these as Infotainment-domain signed commands.
//! The proto structure is just three nested length-delimited message
//! fields with empty leaves, so we can encode the bytes directly
//! without pulling Tesla's whole car_server.proto into our build.
//! Cross-checked against a captured tesla-control `state climate`
//! request that decoded to exactly the 6-byte form we produce here.
//!
//! Wire layout (`state climate` shown; field 3 → other states):
//!   12 04         Action.vehicleAction        (field 2, length 4)
//!     0a 02       VehicleAction.getVehicleData(field 1, length 2)
//!       1a 00     GetVehicleData.getClimateState (field 3, length 0)
//!
//! Same shape for every state in the `GetVehicleData` oneof — only
//! the innermost field number changes.

/// Which state to ask the car for. Field numbers come straight from
/// Tesla's car_server.proto `GetVehicleData` message.
#[derive(Debug, Clone, Copy)]
pub enum VehicleDataState {
    /// `GetChargeState` — battery %, charging rate, range.
    Charge = 2,
    /// `GetClimateState` — interior/exterior temps, HVAC, seat heaters.
    Climate = 3,
    /// `GetDriveState` — shift state, speed, heading.
    Drive = 4,
    /// `GetLocationState` — GPS coords (when authorized).
    Location = 7,
    /// `GetClosuresState` — doors, windows, trunk, frunk, lock state.
    Closures = 8,
    /// `GetTirePressureState` — TPMS PSI per tire.
    TirePressure = 14,
}

impl VehicleDataState {
    /// Parse the CLI / API string form used by tesla-control (`state climate`,
    /// `state charge`, etc.).
    #[allow(clippy::should_implement_trait)] // unknown names map to None, with no error type
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "charge" => Self::Charge,
            "climate" => Self::Climate,
            "drive" => Self::Drive,
            "location" => Self::Location,
            "closures" => Self::Closures,
            "tire-pressure" | "tires" => Self::TirePressure,
            _ => return None,
        })
    }
}

/// Encode one `car_server.Action { vehicleAction { getVehicleData
/// { get<State>State {} } } }` as wire bytes. Three nesting levels:
///
///   Action            field 2 (vehicleAction) →
///   VehicleAction     field 1 (getVehicleData) →
///   GetVehicleData    field <state> (the inner Get*State) →
///   GetXxxState       empty leaf
///
/// Climate (state=3) produces exactly `12 04 0a 02 1a 00` — 6 bytes.
pub fn build_get_state_request(state: VehicleDataState) -> Vec<u8> {
    // GetVehicleData{ getXxxState = GetXxxState{} } — wraps an empty
    // inner message under the state's field number.
    let get_vehicle_data = encode_message_field(state as u32, &[]);
    // Action{ vehicleAction = VehicleAction{ getVehicleData = ... } }
    let vehicle_action = encode_message_field(1, &get_vehicle_data);
    encode_message_field(2, &vehicle_action)
}

/// Write one length-delimited (wire type 2) field as `tag || varint(len) || value`.
fn encode_message_field(field_number: u32, value: &[u8]) -> Vec<u8> {
    let tag = (field_number << 3) | 2;
    let mut buf = Vec::with_capacity(value.len() + 4);
    write_varint(&mut buf, tag as u64);
    write_varint(&mut buf, value.len() as u64);
    buf.extend_from_slice(value);
    buf
}

fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push(((v & 0x7f) | 0x80) as u8);
        v >>= 7;
    }
    buf.push(v as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climate_matches_captured_wire_bytes() {
        // From decrypted plaintext of a captured tesla-control `state
        // climate` signed request: 0x12 0x04 0x0a 0x02 0x1a 0x00.
        // If this assertion fails, the encoding rule changed.
        let bytes = build_get_state_request(VehicleDataState::Climate);
        assert_eq!(hex::encode(&bytes), "12040a021a00");
    }

    #[test]
    fn all_states_are_well_formed() {
        // Each state encodes to a 6-byte outer envelope (field tag +
        // length + 4 inner bytes), as long as the inner-most field
        // tag fits in a single varint byte (field number < 16).
        for state in [
            VehicleDataState::Charge,
            VehicleDataState::Climate,
            VehicleDataState::Drive,
            VehicleDataState::Location,
            VehicleDataState::Closures,
            VehicleDataState::TirePressure,
        ] {
            let b = build_get_state_request(state);
            // Sanity: starts with Action.vehicleAction tag (0x12).
            assert_eq!(b[0], 0x12, "Action.vehicleAction tag");
            // Sanity: parseable round-trip length matches.
            assert_eq!(b.len(), b[1] as usize + 2);
        }
    }

    #[test]
    fn varint_encoder_handles_multibyte() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0x7f);
        assert_eq!(buf, vec![0x7f]);
        buf.clear();
        write_varint(&mut buf, 0x80);
        assert_eq!(buf, vec![0x80, 0x01]);
        buf.clear();
        write_varint(&mut buf, 1871);
        // tesla-control's captured counter; varint = 0xcf 0x0e.
        assert_eq!(buf, vec![0xcf, 0x0e]);
    }
}
