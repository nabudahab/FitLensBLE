// data.rs
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

//Define HR UUID
pub const HR_UUID: u16 = 0x180D;
pub const HR_CHAR_UUID: u16 = 0x2a37;
pub const CPS_UUID: u16 = 0x1818;
pub const CPS_MEASUREMENT_CHAR_UUID: u16 = 0x2A63;

pub const PAIRED_STREAM_HR: u8 = 1 << 0;
pub const PAIRED_STREAM_POWER: u8 = 1 << 1;
pub const PAIRED_STREAM_CADENCE: u8 = 1 << 2;

pub struct HeartRateData {
    pub bpm: u16,
    pub energy: Option<u16>,
}

pub struct CyclingPowerData {
    pub instantaneous_power_w: i16,
    pub cadence_rpm: Option<u16>,
    pub cumulative_crank_revs: Option<u16>,
    pub last_crank_event_time_1024: Option<u16>,
}


//Shared state init between BLE and SPI
pub static TELEMETRY: Signal<CriticalSectionRawMutex, HeartRateData> = Signal::new();
pub static POWER_TELEMETRY: Signal<CriticalSectionRawMutex, CyclingPowerData> = Signal::new();

static PAIRED_STREAMS: AtomicU8 = AtomicU8::new(0);

pub fn reset_paired_streams() {
    PAIRED_STREAMS.store(0, Ordering::Relaxed);
}

pub fn mark_paired_stream(stream_bit: u8) {
    PAIRED_STREAMS.fetch_or(stream_bit, Ordering::Relaxed);
}

pub fn clear_paired_stream(stream_bit: u8) {
    PAIRED_STREAMS.fetch_and(!stream_bit, Ordering::Relaxed);
}

pub fn paired_streams() -> u8 {
    PAIRED_STREAMS.load(Ordering::Relaxed)
}