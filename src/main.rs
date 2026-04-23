#![no_std]
#![no_main]

mod ble;
mod data;
mod hr;
mod power;
mod spi;

use bt_hci::cmd::le::LeSetScanParams;
use bt_hci::controller::ControllerCmdSync;
use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::RNG;
use embassy_nrf::{bind_interrupts, rng};
use embassy_nrf::{peripherals, spim};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;
use trouble_host::gatt::GattClient;
use trouble_host::prelude::DefaultPacketPool;
use trouble_host::{Address, Controller, Host, HostResources};

use {defmt_rtt as _, panic_probe as _};

/// Max number of connections
const CONNECTIONS_MAX: usize = 2;
const L2CAP_CHANNELS_MAX: usize = 4;
const RECONNECT_RETRY_MS: u64 = 250;
const SDC_MEM_BYTES: usize = 8192;

static MPSL_STORAGE: StaticCell<MultiprotocolServiceLayer<'static>> = StaticCell::new();
static RNG_STORAGE: StaticCell<rng::Rng<Async>> = StaticCell::new();
static SDC_MEM_STORAGE: StaticCell<sdc::Mem<SDC_MEM_BYTES>> = StaticCell::new();

fn central_identity_address() -> Address {
    let ficr = embassy_nrf::pac::FICR;
    let mut addr = [0u8; 6];
    let lo = ficr.deviceaddr(0).read();
    let hi = ficr.deviceaddr(1).read();
    addr[0..4].copy_from_slice(&lo.to_le_bytes());
    addr[4..6].copy_from_slice(&hi.to_le_bytes()[0..2]);
    addr[5] |= 0xC0;
    Address::random(addr)
}

fn log_connection_event(event: &trouble_host::prelude::ConnectionEvent) {
    match event {
        trouble_host::prelude::ConnectionEvent::Disconnected { reason } => {
            info!("BLE Event: Disconnected, reason={:?}", reason);
        }
        trouble_host::prelude::ConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
            info!(
                "BLE Event: PhyUpdated, tx={:?}, rx={:?}",
                defmt::Debug2Format(tx_phy),
                defmt::Debug2Format(rx_phy)
            );
        }
        trouble_host::prelude::ConnectionEvent::ConnectionParamsUpdated {
            conn_interval,
            peripheral_latency,
            supervision_timeout,
        } => {
            info!(
                "BLE Event: ConnectionParamsUpdated, interval={:?}, latency={}, timeout={:?}",
                defmt::Debug2Format(conn_interval),
                peripheral_latency,
                defmt::Debug2Format(supervision_timeout)
            );
        }
        trouble_host::prelude::ConnectionEvent::DataLengthUpdated {
            max_tx_octets,
            max_tx_time,
            max_rx_octets,
            max_rx_time,
        } => {
            info!(
                "BLE Event: DataLengthUpdated, tx_octets={}, tx_time={}, rx_octets={}, rx_time={}",
                max_tx_octets, max_tx_time, max_rx_octets, max_rx_time
            );
        }
        trouble_host::prelude::ConnectionEvent::FrameSpaceUpdated {
            frame_space,
            initiator,
            phys,
            spacing_types,
        } => {
            info!(
                "BLE Event: FrameSpaceUpdated, frame_space={:?}, initiator={:?}, phys={:?}, spacing_types={:?}",
                defmt::Debug2Format(frame_space),
                defmt::Debug2Format(initiator),
                defmt::Debug2Format(phys),
                defmt::Debug2Format(spacing_types)
            );
        }
        trouble_host::prelude::ConnectionEvent::ConnectionRateChanged {
            conn_interval,
            subrate_factor,
            peripheral_latency,
            continuation_number,
            supervision_timeout,
        } => {
            info!(
                "BLE Event: ConnectionRateChanged, interval={:?}, subrate={}, latency={}, cont_num={}, timeout={:?}",
                defmt::Debug2Format(conn_interval),
                subrate_factor,
                peripheral_latency,
                continuation_number,
                defmt::Debug2Format(supervision_timeout)
            );
        }
        trouble_host::prelude::ConnectionEvent::RequestConnectionParams(_) => {
            info!("BLE Event: RequestConnectionParams");
        }
    }
}

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

fn build_sdc<'d, const N: usize>(
    p: nrf_sdc::Peripherals<'d>,
    rng: &'d mut rng::Rng<Async>,
    mpsl: &'d MultiprotocolServiceLayer,
    mem: &'d mut sdc::Mem<N>,
) -> Result<nrf_sdc::SoftdeviceController<'d>, nrf_sdc::Error> {
    sdc::Builder::new()?
        .support_scan()
        .support_central()
        .central_count(2)?
        .build(p, rng, mpsl, mem)
}

async fn send_latest_for_profile(
    spim: &mut spim::Spim<'static>,
    ncs: &mut Output<'static>,
    latest_hr_bpm: Option<u16>,
    latest_power_w: Option<i16>,
    latest_cadence_rpm: Option<u16>,
) {
    let profile = crate::data::paired_streams();

    if (profile & crate::data::PAIRED_STREAM_HR) != 0 {
        if let Some(hr_bpm) = latest_hr_bpm {
            crate::spi::send_heart_rate_frame(spim, ncs, hr_bpm).await;
        }
    }

    if (profile & crate::data::PAIRED_STREAM_POWER) != 0 {
        if let Some(power_w) = latest_power_w {
            crate::spi::send_cycling_power_frame(spim, ncs, power_w).await;
        }
    }

    if (profile & crate::data::PAIRED_STREAM_CADENCE) != 0 {
        if let Some(cadence_rpm) = latest_cadence_rpm {
            crate::spi::send_cycling_cadence_frame(spim, ncs, cadence_rpm).await;
        }
    }
}

#[embassy_executor::task]
async fn spi_task(mut spim: spim::Spim<'static>, mut ncs: Output<'static>) {
    let mut latest_hr_bpm: Option<u16> = None;
    let mut latest_power_w: Option<i16> = None;
    let mut latest_cadence_rpm: Option<u16> = None;
    let mut initial_profile_batch_sent = false;
    let mut last_sent_at: Option<Instant> = None;
    let mut last_profile = 0u8;

    loop {
        match with_timeout(
            Duration::from_secs(1),
            select(
                crate::data::TELEMETRY.wait(),
                crate::data::POWER_TELEMETRY.wait(),
            ),
        )
        .await
        {
            Ok(Either::First(hr_data)) => {
                latest_hr_bpm = Some(hr_data.bpm);
            }
            Ok(Either::Second(power_data)) => {
                latest_power_w = Some(power_data.instantaneous_power_w);
                if let Some(cadence_rpm) = power_data.cadence_rpm {
                    latest_cadence_rpm = Some(cadence_rpm);
                }
            }
            Err(_) => {}
        }

        let profile = crate::data::paired_streams();
        if profile != last_profile {
            info!(
                "Paired stream profile updated: hr={} power={} cadence={}",
                (profile & crate::data::PAIRED_STREAM_HR) != 0,
                (profile & crate::data::PAIRED_STREAM_POWER) != 0,
                (profile & crate::data::PAIRED_STREAM_CADENCE) != 0
            );
            last_profile = profile;
            initial_profile_batch_sent = false;
        }

        let hr_ready = (profile & crate::data::PAIRED_STREAM_HR) == 0 || latest_hr_bpm.is_some();
        let power_ready =
            (profile & crate::data::PAIRED_STREAM_POWER) == 0 || latest_power_w.is_some();
        let cadence_ready =
            (profile & crate::data::PAIRED_STREAM_CADENCE) == 0 || latest_cadence_rpm.is_some();

        let has_any_paired_stream = profile != 0;
        let all_paired_data_ready =
            has_any_paired_stream && hr_ready && power_ready && cadence_ready;

        if all_paired_data_ready && !initial_profile_batch_sent {
            send_latest_for_profile(
                &mut spim,
                &mut ncs,
                latest_hr_bpm,
                latest_power_w,
                latest_cadence_rpm,
            )
            .await;
            initial_profile_batch_sent = true;
            last_sent_at = Some(Instant::now());
            info!("Initial SPI batch sent for all currently paired streams");
            continue;
        }

        if initial_profile_batch_sent {
            let should_send = match last_sent_at {
                Some(last) => Instant::now().duration_since(last) >= Duration::from_secs(1),
                None => true,
            };

            if should_send {
                send_latest_for_profile(
                    &mut spim,
                    &mut ncs,
                    latest_hr_bpm,
                    latest_power_w,
                    latest_cadence_rpm,
                )
                .await;
                last_sent_at = Some(Instant::now());
                info!("HR: {} bpm, Power: {} W, Cadence: {} rpm", latest_hr_bpm, latest_power_w, latest_cadence_rpm);
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    //Peripheral init
    info!("Init embassy...");
    cortex_m::asm::delay(32_000_000);
    let mut config = embassy_nrf::config::Config::default();
    config.lfclk_source = embassy_nrf::config::LfclkSource::InternalRC;
    let p = embassy_nrf::init(config);
    info!("Init embassy done.");
    cortex_m::asm::delay(32_000_000);

    info!("Init SPI...");
    let mut config = spim::Config::default();
    config.frequency = spim::Frequency::M8;

    let spim = spim::Spim::new(p.SPI2, Irqs, p.P0_08, p.P0_09, p.P0_10, config);
    let ncs = Output::new(p.P0_11, Level::High, OutputDrive::Standard);
    info!("Init SPI done.");
    cortex_m::asm::delay(32_000_000);

    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: 500,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };
    let mpsl = MPSL_STORAGE.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(
        mpsl_p, Irqs, lfclk_cfg
    )));
    info!("Init MPSL done.");
    cortex_m::asm::delay(32_000_000);
    spawner.spawn(mpsl_task(mpsl).unwrap());

    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );
    let rng = RNG_STORAGE.init(rng::Rng::new(p.RNG, Irqs));
    let sdc_mem = SDC_MEM_STORAGE.init(sdc::Mem::<SDC_MEM_BYTES>::new());
    info!("Init SDC...");
    let sdc = unwrap!(build_sdc(sdc_p, rng, mpsl, sdc_mem));
    info!("Init SDC done.");

    // Run SPI sender in a completely separate task so it doesn't block the executor's BLE runner.
    spawner.spawn(spi_task(spim, ncs).unwrap());

    Timer::after(Duration::from_millis(200)).await;

    ble_run(sdc).await;
}

pub async fn ble_run<C>(controller: C)
where
    C: Controller + ControllerCmdSync<LeSetScanParams>,
{
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let address = central_identity_address();
    info!("Central identity address = {:?}", address);
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);

    let Host {
        central: host_central,
        mut runner,
        ..
    } = stack.build();

    let discover = ble::Discover::new([data::CPS_UUID, data::HR_UUID]);

    let _ = join(runner.run_with_handler(&discover), async {
        let power_channel = embassy_sync::channel::Channel::<
            embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
            trouble_host::prelude::Connection<'_, DefaultPacketPool>,
            1,
        >::new();
        let hr_channel = embassy_sync::channel::Channel::<
            embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
            trouble_host::prelude::Connection<'_, DefaultPacketPool>,
            1,
        >::new();

        let power_active = core::cell::Cell::new(false);
        let hr_active = core::cell::Cell::new(false);

        let central_loop = async {
            let mut central = host_central;
            loop {
                let p_active = power_active.get();
                let h_active = hr_active.get();

                if p_active && h_active {
                    Timer::after_millis(500).await;
                    continue;
                }

                let (found, c) = ble::acquire_any(central, Duration::from_secs(3)).await;
                central = c;

                if let Some((kind, addr, uuid)) = found {
                    if uuid == data::CPS_UUID && !power_active.get() {
                        info!("Discovered power meter target: {:?}", addr);
                        Timer::after_millis(50).await;
                        match ble::connect(&mut central, kind, addr).await {
                            Ok(conn) => {
                                info!("Connected to power sensor, sending to handler");
                                power_active.set(true);
                                discover.set_wanted(data::CPS_UUID, false);
                                let _ = power_channel.send(conn).await;
                            }
                            Err(()) => info!("Power sensor connect failed"),
                        }
                    } else if uuid == data::HR_UUID && !hr_active.get() {
                        info!("Discovered HRM target: {:?}", addr);
                        Timer::after_millis(50).await;
                        match ble::connect(&mut central, kind, addr).await {
                            Ok(conn) => {
                                info!("Connected to HR sensor, sending to handler");
                                hr_active.set(true);
                                discover.set_wanted(data::HR_UUID, false);
                                let _ = hr_channel.send(conn).await;
                            }
                            Err(()) => info!("HR sensor connect failed"),
                        }
                    }
                }
            }
        };

        let power_loop = async {
            loop {
                let conn = power_channel.receive().await;
                info!("Power loop starting GATT client");
                match GattClient::<_, _, 32>::new(&stack, &conn).await {
                    Ok(client) => {
                        let (gatt_task_result, _) = join(
                            client.task(),
                            crate::power::monitor_power_and_cadence(&client, &conn),
                        )
                        .await;

                        if let Err(e) = gatt_task_result {
                            info!(
                                "Power GATT task ended with error: {:?}",
                                defmt::Debug2Format(&e)
                            );
                        }
                    }
                    Err(e) => {
                        info!(
                            "Failed to create power GATT client: {:?}",
                            defmt::Debug2Format(&e)
                        );
                    }
                }
                info!("Power connection ended.");
                crate::data::clear_paired_stream(crate::data::PAIRED_STREAM_POWER);
                crate::data::clear_paired_stream(crate::data::PAIRED_STREAM_CADENCE);
                power_active.set(false);
                discover.set_wanted(data::CPS_UUID, true);
            }
        };

        let hr_loop = async {
            loop {
                let conn = hr_channel.receive().await;
                info!("HR loop starting GATT client");
                match GattClient::<_, _, 32>::new(&stack, &conn).await {
                    Ok(client) => {
                        let (gatt_task_result, _) =
                            join(client.task(), crate::hr::monitor_heart_rate(&client, &conn))
                                .await;

                        if let Err(e) = gatt_task_result {
                            info!(
                                "HR GATT task ended with error: {:?}",
                                defmt::Debug2Format(&e)
                            );
                        }
                    }
                    Err(e) => {
                        info!(
                            "Failed to create HR GATT client: {:?}",
                            defmt::Debug2Format(&e)
                        );
                    }
                }
                info!("HR connection ended.");
                crate::data::clear_paired_stream(crate::data::PAIRED_STREAM_HR);
                hr_active.set(false);
                discover.set_wanted(data::HR_UUID, true);
            }
        };

        embassy_futures::join::join3(central_loop, power_loop, hr_loop).await;
    })
    .await;
}
