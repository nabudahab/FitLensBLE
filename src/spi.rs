use embassy_nrf::spim::Spim;
use embassy_nrf::gpio::Output;

const FRAME_TYPE_HEART_RATE: u8 = 0x01;
const FRAME_TYPE_CYCLING_POWER: u8 = 0x02;
const FRAME_TYPE_CYCLING_CADENCE: u8 = 0x03;
const FRAME_END: u8 = 0xFF;

async fn send_frame(
    spi: &mut Spim<'_>,
    ncs: &mut Output<'_>,
    frame_type: u8,
    payload: [u8; 2],
) {
    ncs.set_low();

    let mut frame = [0u8; 5];
    frame[0] = frame_type;
    frame[1..3].copy_from_slice(&payload);
    frame[3] = 0x34;
    frame[4] = FRAME_END;

    let _ = spi.write(&frame).await;

    ncs.set_high();
}

pub async fn send_heart_rate_frame(
    spi: &mut Spim<'_>,
    ncs: &mut Output<'_>,
    hr_bpm: u16,
) {
    send_frame(spi, ncs, FRAME_TYPE_HEART_RATE, hr_bpm.to_le_bytes()).await;
}

pub async fn send_cycling_power_frame(
    spi: &mut Spim<'_>,
    ncs: &mut Output<'_>,
    power_watts: i16,
) {
    send_frame(spi, ncs, FRAME_TYPE_CYCLING_POWER, power_watts.to_le_bytes()).await;
}

pub async fn send_cycling_cadence_frame(
    spi: &mut Spim<'_>,
    ncs: &mut Output<'_>,
    cadence_rpm: u16,
) {
    send_frame(spi, ncs, FRAME_TYPE_CYCLING_CADENCE, cadence_rpm.to_le_bytes()).await;
}