use embassy_time::{Duration};
use core::cell::RefCell;
use heapless::Deque;
use trouble_host::prelude::{Scanner, ScanConfig, PhySet, BdAddr, EventHandler, LeAdvReportsIter, ConnectConfig, AddrKind, Central};
use trouble_host::connection::{RequestedConnParams, Connection};
use trouble_host::gatt::GattClient;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

pub static DEVICE_FOUND: Signal<CriticalSectionRawMutex, (AddrKind, BdAddr)> = Signal::new();

pub fn search_for_manufacturer_id(data: &[u8]) -> Option<u16> {
    let size = data.len();
    let mut i = 0;
    while i < size.saturating_sub(2) {
        let packet_size = data[i] as usize;
        if packet_size == 0 || i + packet_size + 1 > size {
            break;
        }

        let ad_type = data[i + 1];
        // 0xFF means Manufacturer Specific Data
        if ad_type == 0xFF && packet_size >= 3 {
            let mfg_id = u16::from_le_bytes([data[i + 2], data[i + 3]]);
            return Some(mfg_id);
        }

        i += packet_size + 1;
    }
    None
}

pub fn search_for_uuid(data: &[u8], uuid: u16) -> bool
{
    //convert UUID to 2 bytes in little endian
    let uuid_bytes: [u8; 2] = [(uuid & 0xFF) as u8, (uuid >> 8) as u8];
    let size = data.len();
    if size < 2 {
        return false;
    }
    let mut i = 0;
    while i < size - 2 //loops over a single packet
    {
        let packet_size = data[i] as usize;
        
        if packet_size == 0 {
            break;
        }
        
        if data[i+1] != 0x02 && data[i+1] != 0x03 {
            i += packet_size + 1;
            continue;
        } else {
            //check not only if the UUID exists, but if it starts at an "even" number of bytes from the packet start
            //so we don't accidentally mush two packets together
            let end_idx = (i + packet_size + 1).min(size);
            if end_idx > i + 2 && data[i + 2..end_idx].windows(2).position(|w| w == uuid_bytes).map(|pos| pos % 2 == 0).unwrap_or(false) {
                return true;
            }
            else {
                i += packet_size + 1;
                continue;
            }
        }
    }
    return false;
}

pub struct Discover {
    seen: RefCell<Deque<BdAddr, 128>>,
    is_found: core::cell::Cell<bool>,
    target_uuid: u16,
}

impl Discover {
    pub fn new(target_uuid: u16) -> Self {
        Self {
            seen: RefCell::new(Deque::new()),
            is_found: core::cell::Cell::new(false),
            target_uuid,
        }
    }
}

impl EventHandler for Discover {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
        if self.is_found.get() {
            return;
        }
        let mut seen = self.seen.borrow_mut();
        while let Some(Ok(report)) = it.next() {
            if seen.iter().find(|b| b.raw() == report.addr.raw()).is_none() {
                if search_for_uuid(report.data, self.target_uuid) {
                    if let Some(mfg_id) = search_for_manufacturer_id(report.data) {
                        defmt::info!("Found HR device! Manufacturer ID: {:04X}", mfg_id);
                    } else {
                        defmt::info!("Found HR device! (No manufacturer ID in this packet)");
                    }
                    self.is_found.set(true);
                    DEVICE_FOUND.signal((report.addr_kind, report.addr));
                    return;
                }
                if seen.is_full() {
                    seen.pop_front();
                }
                seen.push_back(report.addr).unwrap();
            }
        }
    }
}

pub async fn acquire<'a, C, P>(central: Central<'a, C, P>) -> (AddrKind, BdAddr, Central<'a, C, P>)
where
    C: trouble_host::Controller + bt_hci::controller::ControllerCmdSync<bt_hci::cmd::le::LeSetScanParams>,
    P: trouble_host::prelude::PacketPool
{
    DEVICE_FOUND.reset();

    let mut config = ScanConfig::default();
    config.active = true;
    config.phys = PhySet::M1;
    config.interval = Duration::from_secs(1);
    config.window = Duration::from_secs(1);

    let mut scanner = Scanner::new(central);
    let _session = scanner.scan(&config).await.unwrap();

    let (kind, addr) = DEVICE_FOUND.wait().await;
    defmt::info!("Scan stopped. Found Target {:?} of kind {:?}", addr, kind);

    drop(_session);
    (kind, addr, scanner.into_inner())
}

pub async fn connect<'a, C, P>(central: &mut Central<'a, C, P>, target_kind: AddrKind, target_addr: BdAddr) -> Result<Connection<'a, P>, ()>
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool
{
    let connect_params = RequestedConnParams {
        min_connection_interval: Duration::from_millis(80),
        max_connection_interval: Duration::from_millis(80),
        max_latency: 0,
        min_event_length: Duration::from_millis(0),
        max_event_length: Duration::from_millis(0),
        supervision_timeout: Duration::from_secs(8),
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

    central.connect(&connect_config).await.map_err(|_e| {
        defmt::error!("Connect error!");
        ()
    })
}

pub async fn read_device_info<C, P, const MAX_ATTRS: usize>(client: &GattClient<'_, C, P, MAX_ATTRS>)
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool
{
    use trouble_host::attribute::Uuid;
    if let Ok(dis_services) = client.services_by_uuid(&Uuid::new_short(0x180a)).await {
        for service in dis_services {
            let mut buf = [0u8; 64];
            if let Ok(len) = client.read_characteristic_by_uuid(&service, &Uuid::new_short(0x2a29), &mut buf).await {
                if let Ok(name) = core::str::from_utf8(&buf[..len]) {
                    defmt::info!("Manufacturer Name: {:?}", name);
                } else {
                    defmt::info!("Manufacturer Name (raw): {:?}", &buf[..len]);
                }
            }
        }
    }
}
