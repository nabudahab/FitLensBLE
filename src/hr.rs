use defmt::info;
use embassy_time::{Duration, with_timeout};
use trouble_host::attribute::{Characteristic, Uuid};
use trouble_host::connection::Connection;
use trouble_host::gatt::GattClient;
use crate::data;

pub async fn monitor_heart_rate<C, P, const MAX_ATTRS: usize>(
    client: &GattClient<'_, C, P, MAX_ATTRS>,
    conn: &Connection<'_, P>,
)
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool
{
    info!("HR monitor started: discovering Heart Rate service");
    match client.services_by_uuid(&Uuid::Uuid16(data::HR_UUID.to_le_bytes())).await {
        Ok(services) => {
            let mut found_hr_service = false;
            for service in services {
                found_hr_service = true;
                info!("Service UUID: {:?}", service.uuid());
                
                info!("Looking for value handle");
                let c: Characteristic<&[u8]> = match client
                    .characteristic_by_uuid(&service, &Uuid::new_short(data::HR_CHAR_UUID))
                    .await
                {
                    Ok(characteristic) => characteristic,
                    Err(e) => {
                        info!("Failed finding HR measurement char: {:?}", defmt::Debug2Format(&e));
                        continue;
                    }
                };

                info!("Subscribing notifications");
                let mut listener = match client.subscribe(&c, false).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        info!("Failed subscribing HR notifications: {:?}", defmt::Debug2Format(&e));
                        continue;
                    }
                };
                info!("Subscribed HR notifications on handle {}", c.handle);
                data::mark_paired_stream(data::PAIRED_STREAM_HR);

                let mut keepalive_timeouts: u8 = 0;

                loop {
                    if !conn.is_connected() {
                        info!("HR monitor exiting: link is disconnected");
                        return;
                    }

                    let data = match with_timeout(Duration::from_secs(2), listener.next()).await {
                        Ok(data) => data,
                        Err(_) => {
                            if !conn.is_connected() {
                                info!("HR monitor exiting after timeout: link is disconnected");
                                return;
                            }

                            keepalive_timeouts = keepalive_timeouts.saturating_add(1);
                            if keepalive_timeouts == 1 {
                                info!("Waiting for HR notifications...");
                            }
                            if keepalive_timeouts >= 4 {
                                let mut scratch = [0u8; 8];
                                match client.read_characteristic(&c, &mut scratch).await {
                                    Ok(read_len) => {
                                        info!("Keepalive read ok: {} bytes", read_len);
                                    }
                                    Err(e) => {
                                        info!("Keepalive read failed: {:?}", defmt::Debug2Format(&e));
                                    }
                                }
                                keepalive_timeouts = 0;
                            }

                            continue;
                        }
                    };

                    keepalive_timeouts = 0;

                    if data.is_indication() {
                        let _ = client.confirm_indication().await;
                        info!("Confirmed ATT indication");
                    }

                    if data.handle() == c.handle {
                        let raw = data.as_ref();
                        let hr_data = parse_hr_packet(raw);
                        data::TELEMETRY.signal(hr_data);
                    } else {
                        info!("HR notification for different handle: got {}, expected {}", data.handle(), c.handle);
                    }
                }
            }

            if !found_hr_service {
                info!("No Heart Rate service (0x180D) found on connected device");
            }
        },
        Err(e) => {
            info!("GATT Error fetching services: {:?}", defmt::Debug2Format(&e));
        }
    }
}

pub fn parse_hr_packet(raw_data: &[u8]) -> data::HeartRateData {
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
            return data::HeartRateData { bpm, energy: Some(energy) };
        },
        (true, false) => {
            //16-bit HR & No Energy
            let bpm = u16::from_le_bytes([raw_data[1], raw_data[2]]);
            return data::HeartRateData {bpm, energy: None};
        },
        (false, true) => {
            //8-bit HR & Energy Present
            let bpm = raw_data[1] as u16;
            let energy = u16::from_le_bytes([raw_data[2], raw_data[3]]);
            return data::HeartRateData {bpm, energy: Some(energy)};
        },
        (false, false) => {
            //8-bit HR & No Energy
            let bpm = raw_data[1] as u16;
            data::HeartRateData {bpm, energy: None}
        }
    }
}