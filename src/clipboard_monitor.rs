use std::thread;

use tokio::sync::mpsc;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorEvent {
    ClipboardChanged,
    Failed,
}

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

// Keeping the proxies alive keeps their Wayland objects registered for events.
#[allow(dead_code)]
enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

impl Manager {
    fn get_data_device(&self, seat: &WlSeat, qh: &QueueHandle<MonitorState>) -> Device {
        match self {
            Self::Ext(manager) => Device::Ext(manager.get_data_device(seat, qh, ())),
            Self::Wlr(manager) => Device::Wlr(manager.get_data_device(seat, qh, ())),
        }
    }
}

struct MonitorState {
    sender: mpsc::Sender<MonitorEvent>,
    _devices: Vec<Device>,
    failed: bool,
}

impl MonitorState {
    fn changed(&self) {
        let _ = self.sender.try_send(MonitorEvent::ClipboardChanged);
    }
}

pub fn spawn(sender: mpsc::Sender<MonitorEvent>) -> Result<(), String> {
    thread::Builder::new()
        .name("clipboard-monitor".into())
        .spawn(move || {
            if let Err(error) = run(sender.clone()) {
                eprintln!("clipboard event monitor stopped: {error}; falling back to polling");
                let _ = sender.blocking_send(MonitorEvent::Failed);
            }
        })
        .map(|_| ())
        .map_err(|error| format!("could not start clipboard event monitor: {error}"))
}

fn run(sender: mpsc::Sender<MonitorEvent>) -> Result<(), String> {
    let connection = Connection::connect_to_env()
        .map_err(|error| format!("could not connect to Wayland: {error}"))?;
    let (globals, mut queue) = registry_queue_init::<MonitorState>(&connection)
        .map_err(|error| format!("could not read Wayland globals: {error}"))?;
    let qh = queue.handle();

    let manager = globals
        .bind(&qh, 1..=1, ())
        .map(Manager::Ext)
        .or_else(|_| globals.bind(&qh, 2..=2, ()).map(Manager::Wlr))
        .map_err(|_| {
            "compositor does not support ext-data-control or wlr-data-control version 2".to_string()
        })?;

    let registry = globals.registry();
    let seats: Vec<WlSeat> = globals.contents().with_list(|globals| {
        globals
            .iter()
            .filter(|global| global.interface == WlSeat::interface().name && global.version >= 2)
            .map(|global| registry.bind(global.name, 2, &qh, ()))
            .collect()
    });
    if seats.is_empty() {
        return Err("Wayland compositor has no seats".into());
    }

    let devices = seats
        .iter()
        .map(|seat| manager.get_data_device(seat, &qh))
        .collect();
    let mut state = MonitorState {
        sender,
        _devices: devices,
        failed: false,
    };
    queue
        .roundtrip(&mut state)
        .map_err(|error| format!("could not initialize clipboard event monitor: {error}"))?;

    loop {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|error| format!("Wayland communication failed: {error}"))?;
        if state.failed {
            return Err("data-control device was stopped by the compositor".into());
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: <WlSeat as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtDataControlManagerV1,
        _event: <ExtDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for MonitorState {
    fn event(
        state: &mut Self,
        _proxy: &ExtDataControlDeviceV1,
        event: <ExtDataControlDeviceV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::Selection { .. }
            | ext_data_control_device_v1::Event::PrimarySelection { .. } => state.changed(),
            ext_data_control_device_v1::Event::Finished => state.failed = true,
            _ => {}
        }
    }

    event_created_child!(MonitorState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ())
    ]);
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for MonitorState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrDataControlDeviceV1,
        event: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::Selection { .. }
            | zwlr_data_control_device_v1::Event::PrimarySelection { .. } => state.changed(),
            zwlr_data_control_device_v1::Event::Finished => state.failed = true,
            _ => {}
        }
    }

    event_created_child!(MonitorState, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ())
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtDataControlOfferV1,
        _event: <ExtDataControlOfferV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for MonitorState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlOfferV1,
        _event: <ZwlrDataControlOfferV1 as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
