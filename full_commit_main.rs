commit 9fd9509c1a5234ccde4f313a0022b09cbc443aa6
Author: nabudahab <nabudaha@purdue.edu>
Date:   Fri Feb 27 09:03:22 2026 -0500

    connect to HRM

diff --git a/src/main.rs b/src/main.rs
index e1e0e6e..f5e1f67 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,39 +1,200 @@
 #![no_std]
 #![no_main]
-#![macro_use]
 
-use core::mem;
+mod hr;
 
-use defmt::{info, *};
+use defmt::{info, unwrap};
 use embassy_executor::Spawner;
-use nrf_softdevice::ble::{central, gatt_client, Address, AddressType};
-use nrf_softdevice::{raw, Softdevice};
+use embassy_futures::join::join;
+use embassy_nrf::mode::Async;
+use embassy_nrf::peripherals::RNG;
+use embassy_nrf::{bind_interrupts, rng};
+use embassy_time::{Duration, Timer};
+use nrf_sdc::mpsl::MultiprotocolServiceLayer;
+use nrf_sdc::{self as sdc, mpsl};
+use static_cell::StaticCell;
+use core::cell::RefCell;
+use heapless::Deque;
+use bt_hci::controller::ControllerCmdSync;
+use bt_hci::cmd::le::LeSetScanParams;
+use trouble_host::{Address, Host, HostResources, Controller};
+use trouble_host::prelude::{DefaultPacketPool, Scanner, ScanConfig, PhySet, BdAddr, EventHandler, LeAdvReportsIter};
+use {defmt_rtt as _, panic_probe as _};
 
-use {panic_halt as _};
+/// Max number of connections
+const CONNECTIONS_MAX: usize = 1;
+const L2CAP_CHANNELS_MAX: usize = 1;
+
+bind_interrupts!(struct Irqs {
+    RNG => rng::InterruptHandler<RNG>;
+    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
+    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
+    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
+    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
+    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
+});
 
 #[embassy_executor::task]
-async fn softdevice_task(sd: &'static Softdevice) -> ! {
-    sd.run().await
+async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
+    mpsl.run().await
 }
 
-#[nrf_softdevice::gatt_client(uuid = "180d")]
-struct HeartRateClient {
-    #[characteristic(uuid = "2a37", notify)]
-    raw_HR: [u8; 5]
-
-    #[characteristic(uuid = "2a39", write)]
-    resetEnergyExpended: u8,
+fn build_sdc<'d, const N: usize>(
+    p: nrf_sdc::Peripherals<'d>,
+    rng: &'d mut rng::Rng<Async>,
+    mpsl: &'d MultiprotocolServiceLayer,
+    mem: &'d mut sdc::Mem<N>,
+) -> Result<nrf_sdc::SoftdeviceController<'d>, nrf_sdc::Error> {
+    sdc::Builder::new()?
+        .support_scan()
+        .support_ext_scan()
+        .support_central()
+        .support_ext_central()
+        .central_count(1)?
+        .build(p, rng, mpsl, mem)
 }
 
 #[embassy_executor::main]
-async fn main(_spawner: Spawner) {
+async fn main(spawner: Spawner) {
+    //Peripheral init
     let p = embassy_nrf::init(Default::default());
-    let mut led = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
 
-    loop {
-        led.set_high();
-        Timer::after_millis(300).await;
-        led.set_low();
-        Timer::after_millis(300).await;
+    //MPSL peripherals (nRF Multiprotocol Service Layer)
+    let mpsl_p = mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
+    
+    //Low frequency clock configuration
+    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
+        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
+        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
+        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
+        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
+        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
+    };
+
+    //Multiprotocol Service Layer, Nordic Prioprietary Radio stuff.
+    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
+    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(mpsl_p, Irqs, lfclk_cfg)));
+    spawner.spawn(unwrap!(mpsl_task(&*mpsl)));
+
+    //SoftDevice Controller peripherals setup
+    let sdc_p = sdc::Peripherals::new(
+        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26,
+        p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
+    );
+
+    //Random number generator for our MAC address
+    let mut rng = rng::Rng::new(p.RNG, Irqs);
+
+    //SDC memory setup
+    let mut sdc_mem = sdc::Mem::<2712>::new();
+    
+    //Init SDC
+    let sdc = unwrap!(build_sdc(sdc_p, &mut rng, mpsl, &mut sdc_mem));
+
+    Timer::after(Duration::from_millis(200)).await;
+
+    //Scanning, pass softdevice controller into run
+    ble_run(sdc).await;
+}
+
+pub async fn ble_run<C>(controller: C)
+where
+    C: Controller + ControllerCmdSync<LeSetScanParams>,
+{
+    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
+    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
+    let address: Address = Address::random([0xff, 0x8f, 0x1b, 0x05, 0xe4, 0xff]);
+
+    info!("Our address = {:?}", address);
+
+    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
+    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
+
+    let Host {
+        central, mut runner, ..
+    } = stack.build();
+
+    let printer = Printer {
+        seen: RefCell::new(Deque::new()),
+    };
+    let mut scanner = Scanner::new(central);
+    let _ = join(runner.run_with_handler(&printer), async {
+        let mut config = ScanConfig::default();
+        config.active = true;
+        config.phys = PhySet::M1;
+        config.interval = Duration::from_secs(1);
+        config.window = Duration::from_secs(1);
+        let mut _session = scanner.scan(&config).await.unwrap();
+        // Scan forever
+        loop {
+            Timer::after(Duration::from_secs(1)).await;
+        }
+    })
+    .await;
+}
+
+struct Printer {
+    seen: RefCell<Deque<BdAddr, 128>>,
+}
+
+impl EventHandler for Printer {
+    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
+        let mut seen = self.seen.borrow_mut();
+        while let Some(Ok(report)) = it.next() {
+            if seen.iter().find(|b| b.raw() == report.addr.raw()).is_none() {
+                info!("discovered: {:?}", report.addr);
+                if search_for_uuid(report.data, 0x180D) {
+                    info!("\n\n\n\n\nFound HRM!!!!!\n\n\n\n\n");
+                }
+                if seen.is_full() {
+                    seen.pop_front();
+                }
+                seen.push_back(report.addr).unwrap();
+            }
+        }
+    }
+}
+
+fn find_uuid(data: &[u8]) -> Option<u16>
+{
+    if data.is_empty() {
+        return None;
+    }
+    if data[1] != 0x03 && data[1] != 0x02 {
+        return find_uuid(&data[data[0] as usize + 1..]);
+    }
+    else {
+        return Some(u16::from_le_bytes([data[2], data[3]]));
+    }
+}
+
+fn search_for_uuid(data: &[u8], uuid: u16) -> bool
+{
+    //convert UUID to 2 bytes in little endian
+    let uuid_bytes: [u8; 2] = [(uuid & 0xFF) as u8, (uuid >> 8) as u8];
+    let size = data.len();
+    if size < 2 {
+        return false;
+    }
+    let mut i = 0;
+    while i < size - 2 //loops over a single packet
+    {
+        let packet_size = data[i] as usize;
+        if data[i+1] != 0x02 && data[i+1] != 0x03 {
+            i += packet_size + 1;
+            continue;
+        } else {
+            //check not only if the UUID exists, but if it starts at an "even" number of bytes from the packet start
+            //so we don't accidentally mush two packets together
+            let end_idx = (i + packet_size + 1).min(size);
+            if end_idx > i + 2 && data[i + 2..end_idx].windows(2).position(|w| w == uuid_bytes).map(|pos| pos % 2 == 0).unwrap_or(false) {
+                return true;
+            }
+            else {
+                i += packet_size + 1;
+                continue;
+            }
+        }
     }
+    return false;
 }
\ No newline at end of file
