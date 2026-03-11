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
use bt_hci::controller::ControllerCmdSync;
use bt_hci::cmd::le::LeSetScanParams;
use trouble_host::{Address, Host, HostResources, Controller};
use trouble_host::prelude::DefaultPacketPool;


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
        .support_ext_scan()
        .support_central()
        .support_ext_central()
        .central_count(1)?
        .build(p, rng, mpsl, mem)
}

fn ble_init(spawner: Spawner, p: embassy_nrf::Peripherals) -> sdc::SoftdeviceController<'static> {
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
    static RNG: StaticCell<rng::Rng<Async>> = StaticCell::new();
    let rng = RNG.init(rng::Rng::new(p.RNG, Irqs));

    //SDC memory setup
    static SDC_MEM: StaticCell<sdc::Mem<2712>> = StaticCell::new();
    let sdc_mem = SDC_MEM.init(sdc::Mem::<2712>::new());
    
    //Init SDC
    unwrap!(build_sdc(sdc_p, rng, mpsl, sdc_mem))
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    //Peripheral init
    let p = embassy_nrf::init(Default::default());

    let sdc = ble_init(spawner, p);

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

    let discover = ble::Discover::new(hr::HR_UUID);
    
    let _ = join(runner.run_with_handler(&discover), async {
        let (target_kind, target_addr, mut central) = ble::acquire(central).await;
        
        // Let the scanner cancel event propagate to the controller 
        // to avoid Command Disallowed on LeCreateConn
        Timer::after_millis(50).await;
        
        match ble::connect(&mut central, target_kind, target_addr).await {
            Ok(conn) => {
                info!("Connected to HRM!");
                
                //Define GATT client
                Timer::after(Duration::from_millis(500)).await;
                match GattClient::<_, _, 32>::new(&stack, &conn).await {
                    Ok(client) => {
                        let _ = join(client.task(), async {
                            info!("Fetching all services...");
                            crate::ble::read_device_info(&client).await;
                            crate::hr::monitor_heart_rate(&client).await;
                        }).await;                        
                    },
                    Err(e) => {
                        info!("Failed to create GATT client: {:?}", defmt::Debug2Format(&e));
                    }
                }
            },
            Err(()) => info!("Error Connecting"),
        }
    })
    .await;
}