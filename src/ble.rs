use embassy_time::{Duration, Timer, with_timeout};
use core::cell::RefCell;
use heapless::Deque;
use trouble_host::prelude::{Scanner, ScanConfig, PhySet, BdAddr, EventHandler, LeAdvReportsIter, ConnectConfig, AddrKind, Central};
use trouble_host::connection::{RequestedConnParams, Connection};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use crate::data;

pub static DEVICE_FOUND: Signal<CriticalSectionRawMutex, (AddrKind, BdAddr, u16)> = Signal::new();

fn target_label(uuid: u16) -> &'static str {
    match uuid {
        data::CPS_UUID => "power meter",
        data::HR_UUID => "HRM",
        _ => "unknown target",
    }
}

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
    target_uuids: [u16; 2],
    wanted_mask: core::sync::atomic::AtomicU8,
}

impl Discover {
    pub fn new(target_uuids: [u16; 2]) -> Self {
        Self {
            seen: RefCell::new(Deque::new()),
            target_uuids,
            wanted_mask: core::sync::atomic::AtomicU8::new(3), // Both active
        }
    }

    pub fn set_wanted(&self, uuid: u16, wanted: bool) {
        let mut mask = self.wanted_mask.load(core::sync::atomic::Ordering::Relaxed);
        for (i, t_uuid) in self.target_uuids.iter().enumerate() {
            if *t_uuid == uuid {
                if wanted {
                    mask |= 1 << i;
                } else {
                    mask &= !(1 << i);
                }
            }
        }
        self.wanted_mask.store(mask, core::sync::atomic::Ordering::Relaxed);
    }
}

impl EventHandler for Discover {
    fn on_adv_reports(&self, mut it: LeAdvReportsIter<'_>) {
        let mut seen = self.seen.borrow_mut();
        while let Some(Ok(report)) = it.next() {
            if seen.iter().find(|b| b.raw() == report.addr.raw()).is_none() {
                let mut found_uuid = None;
                for uuid in self.target_uuids {
                    if search_for_uuid(report.data, uuid) {
                        found_uuid = Some(uuid);
                        break;
                    }
                }

                if let Some(found_uuid) = found_uuid {
                    let mask = self.wanted_mask.load(core::sync::atomic::Ordering::Relaxed);
                    let mut is_wanted = false;
                    for (i, t_uuid) in self.target_uuids.iter().enumerate() {
                        if *t_uuid == found_uuid && (mask & (1 << i)) != 0 {
                            is_wanted = true;
                            break;
                        }
                    }

                    if is_wanted {
                        let target = target_label(found_uuid);
                        if let Some(mfg_id) = search_for_manufacturer_id(report.data) {
                            defmt::info!("Found target BLE device ({})! Manufacturer ID: {:04X}", target, mfg_id);
                        } else {
                            defmt::info!("Found target BLE device ({})! (No manufacturer ID in this packet)", target);
                        }
                        DEVICE_FOUND.signal((report.addr_kind, report.addr, found_uuid));
                    }
                    
                    // Always return early for our target UUIDs. This prevents them from
                    // polluting the `seen` cache, allowing them to be rediscovered immediately
                    // after disconnecting.
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

pub async fn acquire<'a, C, P>(central: Central<'a, C, P>, target_uuid: u16) -> (AddrKind, BdAddr, Central<'a, C, P>)
where
    C: trouble_host::Controller + bt_hci::controller::ControllerCmdSync<bt_hci::cmd::le::LeSetScanParams>,
    P: trouble_host::prelude::PacketPool
{
    DEVICE_FOUND.reset();

    let mut config = ScanConfig::default();
    config.active = true;
    config.phys = PhySet::M1;
    config.interval = Duration::from_millis(100);
    config.window = Duration::from_millis(50);

    let mut scanner = Scanner::new(central);

    let (kind, addr) = loop {
        let active_found = {
            match scanner.scan(&config).await {
                Ok(session) => {
                    let found = loop {
                        let found = DEVICE_FOUND.wait().await;
                        if found.2 == target_uuid {
                            break found;
                        }
                    };
                    drop(session);
                    Some(found)
                }
                Err(_) => None,
            }
        };

        if let Some((kind, addr, _)) = active_found {
            defmt::info!(
                "Scan stopped. Found {} target {:?} of kind {:?}",
                target_label(target_uuid),
                addr,
                kind
            );
            break (kind, addr);
        }

        defmt::warn!("Active scan start failed, retrying with passive fallback");

        let mut fallback = ScanConfig::default();
        fallback.active = false;
        fallback.phys = PhySet::M1;
        fallback.interval = Duration::from_millis(100);
        fallback.window = Duration::from_millis(50);

        let fallback_found = {
            match scanner.scan(&fallback).await {
                Ok(session) => {
                    let found = loop {
                        let found = DEVICE_FOUND.wait().await;
                        if found.2 == target_uuid {
                            break found;
                        }
                    };
                    drop(session);
                    Some(found)
                }
                Err(_) => None,
            }
        };

        if let Some((kind, addr, _)) = fallback_found {
            defmt::info!(
                "Fallback scan found {} target {:?} kind {:?}",
                target_label(target_uuid),
                addr,
                kind
            );
            break (kind, addr);
        }

        Timer::after_millis(500).await;
    };

    let central = scanner.into_inner();
    (kind, addr, central)
}

pub async fn acquire_any<'a, C, P>(
    central: Central<'a, C, P>,
    scan_timeout: Duration,
) -> (Option<(AddrKind, BdAddr, u16)>, Central<'a, C, P>)
where
    C: trouble_host::Controller + bt_hci::controller::ControllerCmdSync<bt_hci::cmd::le::LeSetScanParams>,
    P: trouble_host::prelude::PacketPool,
{
    DEVICE_FOUND.reset();

    let mut config = ScanConfig::default();
    config.active = true;
    config.phys = PhySet::M1;
    config.interval = Duration::from_millis(100);
    config.window = Duration::from_millis(50);

    let mut scanner = Scanner::new(central);

    let mut found = match scanner.scan(&config).await {
        Ok(session) => {
            let found = with_timeout(scan_timeout, DEVICE_FOUND.wait()).await.ok();
            drop(session);
            found
        }
        Err(_) => None,
    };

    if found.is_none() {
        let mut fallback = ScanConfig::default();
        fallback.active = false;
        fallback.phys = PhySet::M1;
        fallback.interval = Duration::from_millis(100);
        fallback.window = Duration::from_millis(50);

        found = match scanner.scan(&fallback).await {
            Ok(session) => {
                let found = with_timeout(scan_timeout, DEVICE_FOUND.wait()).await.ok();
                drop(session);
                found
            }
            Err(_) => None,
        };
    }

    let central = scanner.into_inner();
    (found, central)
}

pub async fn connect<'a, C, P>(central: &mut Central<'a, C, P>, target_kind: AddrKind, target_addr: BdAddr) -> Result<Connection<'a, P>, ()>
where
    C: trouble_host::Controller,
    P: trouble_host::prelude::PacketPool
{
    let connect_params = RequestedConnParams {
        min_connection_interval: Duration::from_millis(30),
        max_connection_interval: Duration::from_millis(45),
        max_latency: 0,
        min_event_length: Duration::from_millis(0),
        max_event_length: Duration::from_millis(0),
        supervision_timeout: Duration::from_secs(4),
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
