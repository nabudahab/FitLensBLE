#![no_std]
#![no_main]

mod hr;
mod ble;

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::RNG;
use embassy_nrf::{bind_interrupts, rng};
use embassy_time::{Duration, Timer};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;
use trouble_host::gatt::GattClient;
use trouble_host::attribute::Characteristic;
use core::cell::RefCell;
use heapless::Deque;
use bt_hci::controller::ControllerCmdSync;
use bt_hci::cmd::le::LeSetScanParams;
use trouble_host::{Address, Host, HostResources, Controller};
use trouble_host::prelude::{DefaultPacketPool, Scanner, ScanConfig, PhySet, BdAddr, EventHandler, LeAdvReportsIter, ConnectConfig, AddrKind};
use trouble_host::connection::RequestedConnParams;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use trouble_host::attribute::Uuid;


use {defmt_rtt as _, panic_probe as _};

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
});

// Stop scan and ignore processing if a signal has been found
#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

static HRM_FOUND: Signal<CriticalSectionRawMutex, (AddrKind, BdAddr)> = Signal::new();

fn build_sdc<'d, const N: usize>(
    p: nrf_sdc::Peripherals<'d>,
    rng: &'d mut rng::Rng<Async>,
    mpsl: &'d MultiprotocolServiceLayer,
    mem: &'d mut sdc::Mem<N>,
) -> Result<nrf_sdc::SoftdeviceController<'d>, nrf_sdc::Error> {
    sdc::Builder::new()?
        .support_scan()
        .support_ext_scan()
        .support_central()
        .support_ext_central()
        .central_count(1)?
        .build(p, rng, mpsl, mem)
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    //Peripheral init
    let p = embassy_nrf::init(Default::default());

    //MPSL peripherals (nRF Multiprotocol Service Layer)
    let mpsl_p = mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    
    //Low frequency clock configuration
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };

    //Multiprotocol Service Layer, Nordic Prioprietary Radio stuff.
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(mpsl_p, Irqs, lfclk_cfg)));
    spawner.spawn(unwrap!(mpsl_task(&*mpsl)));

    //SoftDevice Controller peripherals setup
    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26,
        p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    //Random number generator for our MAC address
    let mut rng = rng::Rng::new(p.RNG, Irqs);

    //SDC memory setup
    let mut sdc_mem = sdc::Mem::<2712>::new();
    
    //Init SDC
    let sdc = unwrap!(build_sdc(sdc_p, &mut rng, mpsl, &mut sdc_mem));

    Timer::after(Duration::from_millis(200)).await;

    //Scanning, pass softdevice controller into run
    ble_run(sdc).await;
}

pub async fn ble_run<C>(controller: C)
where
    C: Controller + ControllerCmdSync<LeSetScanParams>,
{
    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
    let address: Address = Address::random([0xff, 0x8f, 0x1b, 0x05, 0xe4, 0xff]);

    info!("Our address = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);

    let Host {
        central, mut runner, ..
    } = stack.build();

    let discover = Discover {
        is_found: core::cell::Cell::new(false)
    };
    
    let _ = join(runner.run_with_handler(&discover), async {
        HRM_FOUND.reset();

        let mut config = ScanConfig::default();
        config.active = true;
        config.phys = PhySet::M1;
        config.interval = Duration::from_secs(1);
        config.window = Duration::from_secs(1);

        //Get the HRM address
        let (target_kind, target_addr, mut central) = {
            let mut scanner = Scanner::new(central);
            let mut _session = scanner.scan(&config).await.unwrap();

            //Wait for target addr to show up and then stop scanning
            let (kind, addr) = HRM_FOUND.wait().await;
            info!("Scan stopped. Found HRM Target {:?} of kind {:?}", addr, kind);

            //Drop session to release the borrow
            drop(_session);
            //give central back
            let central = scanner.into_inner();
            (kind, addr, central)
        };
        
        // Let the scanner cancel event propagate to the controller 
        // to avoid Command Disallowed on LeCreateConn
        Timer::after_millis(50).await;
        
        //Connect to the HRM.

        //Create connection parameters
        let connect_params = RequestedConnParams {
            min_connection_interval: Duration::from_millis(80),
            max_connection_interval: Duration::from_millis(80),
            max_latency: 0,
            min_event_length: Duration::from_millis(0),
            max_event_length: Duration::from_millis(0),
            supervision_timeout: Duration::from_secs(8)
        };

        //create connection object with target address and parameters
        let accept = (target_kind, &target_addr);
        let connect_config = ConnectConfig {
            scan_config: ScanConfig {
                filter_accept_list: core::slice::from_ref(&accept),
                interval: Duration::from_millis(60),
                window: Duration::from_millis(60),
                ..Default::default()
            },
            connect_params,
        };

        match central.connect(&connect_config).await {
            Ok(conn) => {
                info!("Connected to HRM!");
                
                //Define GATT client
                Timer::after(Duration::from_millis(500)).await;
                match GattClient::<_, _, 32>::new(&stack, &conn).await {
                    Ok(client) => {
                        let _ = join(client.task(), async {
                            info!("Fetching all services...");

                            // Fetch Device Information Service to get the Manufacturer Name
                            if let Ok(dis_services) = client.services_by_uuid(&Uuid::new_short(0x180a)).await {
                                for service in dis_services {
                                    let mut buf = [0u8; 64];
                                    if let Ok(len) = client.read_characteristic_by_uuid(&service, &Uuid::new_short(0x2a29), &mut buf).await {
                                        if let Ok(name) = core::str::from_utf8(&buf[..len]) {
                                            info!("Manufacturer Name: {:?}", name);
                                        } else {
                                            info!("Manufacturer Name (raw): {:?}", &buf[..len]);
                                        }
                                    }
                                }
                            }

                            match client.services_by_uuid(&Uuid::Uuid16(hr::HR_UUID.to_le_bytes())).await {
                                Ok(services) => {
                                    for service in services {
                                        info!("Service UUID: {:?}", service.uuid());
                                        
                                        info!("Looking for value handle");
                                        let c: Characteristic<&[u8]> = client
                                            .characteristic_by_uuid(&service, &Uuid::new_short(hr::HR_CHAR_UUID))
                                            .await
                                            .unwrap();

                                        info!("Subscribing notifications");
                                        let mut listener = client.subscribe(&c, false).await.unwrap();

                                        loop {
                                            let data = listener.next().await;
                                            if data.handle() == c.handle {
                                                let raw = data.as_ref();
                                                let hr = hr::parse_hr_packet(raw).bpm;
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
                        }).await;                        
                    },
                    Err(e) => {
                        info!("Failed to create GATT client: {:?}", defmt::Debug2Format(&e));
                    }
                }
            },
            Err(e) => info!("Error Connecting: {:?}", defmt::Debug2Format(&e)),
        }
    })
    .await;
}

struct Discover {
    is_found: core::cell::Cell<bool>,
}

impl EventHandler for Discover {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
    
    if self.is_found.get()
    {
        return;
    }
    
    while let Some(Ok(report)) = it.next() {
            // info!("discovered: {:?}", report.addr);
            if ble::search_for_uuid(report.data, hr::HR_UUID) {
                if let Some(mfg_id) = ble::search_for_manufacturer_id(report.data) {
                    defmt::info!("Found HR device! Manufacturer ID: {:04X}", mfg_id);
                } else {
                    defmt::info!("Found HR device! (No manufacturer ID in this packet)");
                }
                
                //Place report in mutex for main function to read
                self.is_found.set(true);
                HRM_FOUND.signal((report.addr_kind, report.addr));

                return;
            }
        }
    }
}