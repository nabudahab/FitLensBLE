//Define HR UUID
pub const HR_UUID: u16 = 0x180D;
pub const HR_CHAR_UUID: u16 = 0x2a37;

pub struct HeartRateData {
    pub bpm: u16,
    pub energy: Option<u16>,
}

pub fn parse_hr_packet(raw_data: &[u8]) -> HeartRateData {
    let flag_byte = raw_data[0];
    
    //Check flags
    let hr_is_u16 = (flag_byte & 0x01) != 0;
    let _skin_contact_supported = (flag_byte & 0x02) != 0;
    let _skin_contact_detected = (flag_byte & 0x04) != 0;
    let energy_expended_present = (flag_byte & 0x08) != 0;

    match (hr_is_u16, energy_expended_present) {
        (true, true) => {
            //16-bit HR & Energy Present
            let bpm = u16::from_le_bytes([raw_data[1], raw_data[2]]);
            let energy = u16::from_le_bytes([raw_data[3], raw_data[4]]);
            return HeartRateData { bpm, energy: Some(energy) };
        },
        (true, false) => {
            //16-bit HR & No Energy
            let bpm = u16::from_le_bytes([raw_data[1], raw_data[2]]);
            return HeartRateData {bpm, energy: None};
        },
        (false, true) => {
            //8-bit HR & Energy Present
            let bpm = raw_data[1] as u16;
            let energy = u16::from_le_bytes([raw_data[2], raw_data[3]]);
            return HeartRateData {bpm, energy: Some(energy)};
        },
        (false, false) => {
            //8-bit HR & No Energy
            let bpm = raw_data[1] as u16;
            HeartRateData {bpm, energy: None}
        }
    }
}