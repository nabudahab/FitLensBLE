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
use trouble_host::prelude::{DefaultPacketPool, Scanner, ScanConfig, PhySet, BdAd
dr, EventHandler, LeAdvReportsIter, ConnectConfig, AddrKind};                   use trouble_host::connection::RequestedConnParams;
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

static HRM_FOUND: Signal<CriticalSectionRawMutex, (AddrKind, BdAddr)> = Signal::
new();                                                                          
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
    let mpsl_p = mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.
PPI_CH30, p.PPI_CH31);                                                              
    //Low frequency clock configuration.
    // Try to use the XTAL crystal if available! It is far more precise and avoi
ds Synchronization Timeouts                                                         // during early Connection events. If your custom board DOES NOT have a 32.7
68kHz crystal, you must                                                             // change it back to RC, but you might need more frequent calibrations.
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_XTAL as u8,
        rc_ctiv: 0,
        rc_temp_ctiv: 0,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: false,
    };

    //Multiprotocol Service Layer, Nordic Prioprietary Radio stuff.
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(mpsl_p, Ir
qs, lfclk_cfg)));                                                                   spawner.spawn(unwrap!(mpsl_task(&*mpsl)));

    //SoftDevice Controller peripherals setup
    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, 
p.PPI_CH24, p.PPI_CH25, p.PPI_CH26,                                                     p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
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
    // Using a fixed "random" address can be useful for testing. In real scenari
os, one would                                                                       // use e.g. the MAC 6 byte array as the address (how to get that varies by t
he platform).                                                                       let address: Address = Address::random([0xff, 0x8f, 0x1b, 0x05, 0xe4, 0xff])
;                                                                               
    info!("Our address = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_C
HANNELS_MAX> = HostResources::new();                                                let stack = trouble_host::new(controller, &mut resources).set_random_address
(address);                                                                      
    let Host {
        central, mut runner, ..
    } = stack.build();

    let discover = Discover {
        seen: RefCell::new(Deque::new()),
        is_found: core::cell::Cell::new(false),
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
            info!("Scan stopped. Found HRM Target {:?} of kind {:?}", addr, kind
);                                                                              
            //Drop session to release the borrow
            drop(_session);
            //give central back
            let central = scanner.into_inner();
            (kind, addr, central)
        };
        
        //Connect to the HRM wait for the scanner to fully quiet down
        Timer::after_millis(150).await;

        // Use flexible connection parameters, default is strict 80ms/80ms which
 many devices reject.                                                                   let connect_params = RequestedConnParams {
            min_connection_interval: Duration::from_millis(15),
            max_connection_interval: Duration::from_millis(60),
            max_latency: 0,
            min_event_length: Duration::from_millis(0),
            max_event_length: Duration::from_millis(0),
            supervision_timeout: Duration::from_millis(4000),
        };
        
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
                
                match GattClient::<_, _, 32>::new(&stack, &conn).await {
                    Ok(client) => {
                        let _ = join(client.task(), async {
                            info!("Fetching all services...");
                            match client.services_by_uuid(&Uuid::Uuid16(hr::HR_U
UID.to_le_bytes())).await {                                                                                     Ok(services) => {
                                    for service in services {
                                        // info!("Service UUID: {:?}", service.u
uid());                                                                                                                 
                                        // info!("Looking for value handle");
                                        let c: Characteristic<&[u8]> = client
                                            .characteristic_by_uuid(&service, &U
uid::new_short(hr::HR_CHAR_UUID))                                                                                           .await
                                            .unwrap();

                                        info!("Subscribing notifications");
                                        let mut listener = client.subscribe(&c, 
false).await.unwrap();                                                          
                                        loop {
                                            let data = listener.next().await;
                                            let data = data.as_ref();
                                            // info!("Got notification length: {
}", data.len());                                                                                                            
                                            // Make sure we have enough data (at
 least 2 bytes: flags + hr)                                                                                                 if data.len() >= 2 {
                                                // Because parse_hr_packet expec
ts [u8; 5], we can pad it or modify parse_hr_packet                                                                             // For now, let's create a padde
d array to prevent panic                                                                                                        let mut buf = [0u8; 5];
                                                let len = data.len().min(5);
                                                buf[..len].copy_from_slice(&data
[..len]);                                                                                                                       
                                                let hr_data = hr::parse_hr_packe
t(buf);                                                                                                                         info!("Heart Rate: {} bpm", hr_d
ata.bpm);                                                                                                                       if let Some(energy) = hr_data.en
ergy {                                                                                                                              info!("Energy Expanded: {} k
J", energy);                                                                                                                    }
                                            }
                                        }
                                    }
                                },
                                Err(e) => {
                                    info!("GATT Error fetching services: {:?}", 
defmt::Debug2Format(&e));                                                                                       }
                            }
                        }).await;                        
                    },
                    Err(e) => {
                        info!("Failed to create GATT client: {:?}", defmt::Debug
2Format(&e));                                                                                       }
                }

                // Note: The `client.subscribe` call above already writes 0x0001
 to the CCCD automatically.                                                                     // You do not need to manually write `enable_notify` to the CCCD
.                                                                               
            },
            Err(e) => info!("Error Connecting: {:?}", defmt::Debug2Format(&e)),
        }
    })
    .await;
}

struct Discover {
    seen: RefCell<Deque<BdAddr, 128>>,
    is_found: core::cell::Cell<bool>,
}

impl EventHandler for Discover {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
        if self.is_found.get()
        {
            return;
        }
        let mut seen = self.seen.borrow_mut();
        while let Some(Ok(report)) = it.next() {
            if seen.iter().find(|b| b.raw() == report.addr.raw()).is_none() {
                // info!("discovered: {:?}", report.addr);
                if ble::search_for_uuid(report.data, hr::HR_UUID) {
                    //Place report in mutex for main function to read
                    self.is_found.set(true);
                    HRM_FOUND.signal((report.addr_kind, report.addr));

                    return;
                }
                if seen.is_full() {
                    seen.pop_front();
                }
                seen.push_back(report.addr).unwrap();
            }
        }
    }
