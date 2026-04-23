use defmt::info;
use embassy_time::{with_timeout, Duration};
use trouble_host::attribute::{Characteristic, Uuid};
use trouble_host::connection::Connection;
use trouble_host::gatt::GattClient;

use crate::data;

#[derive(Clone, Copy)]
struct CrankState {
    cumulative_crank_revs: u16,
    last_crank_event_time_1024: u16,
}

pub async fn monitor_power_and_cadence<C, P, const MAX_ATTRS: usize>(
    client: &GattClient<'_, C, P, MAX_ATTRS>,
    conn: &Connection<'_, P>,
)
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool,
{
    match client
        .services_by_uuid(&Uuid::Uuid16(data::CPS_UUID.to_le_bytes()))
        .await
    {
        Ok(services) => {
            for service in services {
                info!("CPS Service UUID: {:?}", service.uuid());

                let c: Characteristic<&[u8]> = match client
                    .characteristic_by_uuid(&service, &Uuid::new_short(data::CPS_MEASUREMENT_CHAR_UUID))
                    .await
                {
                    Ok(characteristic) => characteristic,
                    Err(e) => {
                        info!("Failed finding CPS measurement char: {:?}", defmt::Debug2Format(&e));
                        continue;
                    }
                };

                info!("Subscribing CPS measurement notifications");
                let mut listener = match client.subscribe(&c, false).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        info!("Failed subscribing CPS measurement: {:?}", defmt::Debug2Format(&e));
                        continue;
                    }
                };
                data::mark_paired_stream(data::PAIRED_STREAM_POWER);

                let mut prev_crank_state: Option<CrankState> = None;

                loop {
                    if !conn.is_connected() {
                        info!("Power monitor exiting: link disconnected");
                        return;
                    }

                    let data = match with_timeout(Duration::from_secs(3), listener.next()).await {
                        Ok(data) => data,
                        Err(_) => {
                            if !conn.is_connected() {
                                info!("Power monitor exiting after timeout: link disconnected");
                                return;
                            }
                            continue;
                        }
                    };

                    if data.is_indication() {
                        let _ = client.confirm_indication().await;
                    }

                    if data.handle() != c.handle {
                        continue;
                    }

                    let raw = data.as_ref();
                    if let Some((sample, next_state)) = parse_cycling_power_measurement(raw, prev_crank_state) {
                        prev_crank_state = next_state;

                        if sample.cadence_rpm.is_some() {
                            data::mark_paired_stream(data::PAIRED_STREAM_CADENCE);
                        }

                        data::POWER_TELEMETRY.signal(sample);
                    }
                }
            }
        }
        Err(e) => {
            info!("GATT Error fetching CPS services: {:?}", defmt::Debug2Format(&e));
        }
    }
}

fn parse_cycling_power_measurement(
    raw: &[u8],
    prev_crank_state: Option<CrankState>,
) -> Option<(data::CyclingPowerData, Option<CrankState>)> {
    if raw.len() < 4 {
        return None;
    }

    let flags = u16::from_le_bytes([raw[0], raw[1]]);
    let instantaneous_power_w = i16::from_le_bytes([raw[2], raw[3]]);

    let mut offset = 4usize;

    if (flags & (1 << 0)) != 0 {
        if raw.len() < offset + 1 {
            return None;
        }
        offset += 1;
    }

    if (flags & (1 << 2)) != 0 {
        if raw.len() < offset + 2 {
            return None;
        }
        offset += 2;
    }

    if (flags & (1 << 4)) != 0 {
        if raw.len() < offset + 6 {
            return None;
        }
        offset += 6;
    }

    let mut cadence_rpm = None;
    let mut crank_state = None;

    if (flags & (1 << 5)) != 0 {
        if raw.len() < offset + 4 {
            return None;
        }

        let cumulative_crank_revs = u16::from_le_bytes([raw[offset], raw[offset + 1]]);
        let last_crank_event_time_1024 = u16::from_le_bytes([raw[offset + 2], raw[offset + 3]]);

        let current = CrankState {
            cumulative_crank_revs,
            last_crank_event_time_1024,
        };

        cadence_rpm = compute_cadence_rpm(prev_crank_state, current);
        crank_state = Some(current);
    }

    let sample = data::CyclingPowerData {
        instantaneous_power_w,
        cadence_rpm,
        cumulative_crank_revs: crank_state.map(|s| s.cumulative_crank_revs),
        last_crank_event_time_1024: crank_state.map(|s| s.last_crank_event_time_1024),
    };

    Some((sample, crank_state))
}

fn compute_cadence_rpm(prev: Option<CrankState>, current: CrankState) -> Option<u16> {
    let prev = prev?;

    let delta_revs = current
        .cumulative_crank_revs
        .wrapping_sub(prev.cumulative_crank_revs) as u32;
    let delta_time = current
        .last_crank_event_time_1024
        .wrapping_sub(prev.last_crank_event_time_1024) as u32;

    if delta_time == 0 {
        return None;
    }

    if delta_revs == 0 {
        return Some(0);
    }

    let rpm = (delta_revs * 60 * 1024) / delta_time;
    Some(rpm.min(u16::MAX as u32) as u16)
}
