#![no_std]
#![no_main]
#![macro_use]

use core::mem;

use defmt::{info, *};
use embassy_executor::Spawner;
use nrf_softdevice::ble::{central, gatt_client, Address, AddressType};
use nrf_softdevice::{raw, Softdevice};

use {panic_halt as _};

#[embassy_executor::task]
async fn softdevice_task(sd: &'static Softdevice) -> ! {
    sd.run().await
}

#[nrf_softdevice::gatt_client(uuid = "180d")]
struct HeartRateClient {
    #[characteristic(uuid = "2a37", notify)]
    raw_HR: [u8; 5]

    #[characteristic(uuid = "2a39", write)]
    resetEnergyExpended: u8,
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());
    let mut led = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);

    loop {
        led.set_high();
        Timer::after_millis(300).await;
        led.set_low();
        Timer::after_millis(300).await;
    }
}