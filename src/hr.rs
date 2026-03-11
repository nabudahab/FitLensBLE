//Define HR UUID
pub const HR_UUID: u16 = 0x180D;
pub const HR_CHAR_UUID: u16 = 0x2a37;

pub struct HeartRateData {
    pub bpm: u16,
    pub energy: Option<u16>,
}

use defmt::info;
use trouble_host::attribute::{Characteristic, Uuid};
use trouble_host::gatt::GattClient;

pub async fn monitor_heart_rate<C, P, const MAX_ATTRS: usize>(client: &GattClient<'_, C, P, MAX_ATTRS>)
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool
{
    match client.services_by_uuid(&Uuid::Uuid16(HR_UUID.to_le_bytes())).await {
        Ok(services) => {
            for service in services {
                info!("Service UUID: {:?}", service.uuid());
                
                info!("Looking for value handle");
                let c: Characteristic<&[u8]> = client
                    .characteristic_by_uuid(&service, &Uuid::new_short(HR_CHAR_UUID))
                    .await
                    .unwrap();

                info!("Subscribing notifications");
                let mut listener = client.subscribe(&c, false).await.unwrap();

                loop {
                    let data = listener.next().await;
                    if data.handle() == c.handle {
                        let raw = data.as_ref();
                        let hr = parse_hr_packet(raw).bpm;
                        info!("Heartrate: {:?} bpm", hr);
                    } else {
                        info!("Got notification for different handle: {}, expected {}", data.handle(), c.handle);
                    }
                }
            }
        },
        Err(e) => {
            info!("GATT Error fetching services: {:?}", defmt::Debug2Format(&e));
        }
    }
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